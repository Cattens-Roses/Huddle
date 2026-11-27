use super::*;
use serde_json::json;

async fn sync_rules(server: &Server, client: &Client, muted: bool) {
    let mut rules = serde_json::to_value(matrix_sdk::ruma::push::Ruleset::server_default(
        client.user_id().unwrap(),
    ))
    .unwrap();
    if muted {
        rules["override"].as_array_mut().unwrap().insert(0, json!({
            "rule_id":"!admin:example.org", "default":false,"enabled":true,
            "conditions":[{"kind":"event_match","key":"room_id","pattern":"!admin:example.org"}],
            "actions":[]
        }));
    }
    server.traffic.lock().unwrap().sync = Some(json!({"next_batch":format!("mute-{muted}"),
        "account_data":{"events":[{"type":"m.push_rules","content":{"global":rules}}]}
    }));
    client.sync_once(SyncSettings::default()).await.unwrap();
}

#[tokio::test]
async fn mute_is_saved_as_a_matrix_override_and_unmute_restores_defaults() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = room_admin_tests::setup(false).await;
        let (outbox, mut updates) = ui::unbounded();
        handle(
            &client,
            &outbox,
            &Arc::new(Shared::default()),
            Command::MuteRoom {
                room: id.clone(),
                muted: true,
            },
        )
        .await
        .unwrap();
        let Some(Update::RoomMuted { user, room, result }) = updates.next().await else {
            panic!("Expected mute result");
        };
        assert_eq!(user, client.user_id().unwrap());
        assert_eq!(room, id);
        assert_eq!(result.unwrap(), true);
        {
            let traffic = server.traffic.lock().unwrap();
            let (method, path, body) = traffic.push_requests.first().unwrap();
            assert_eq!(method, "PUT");
            assert!(path.contains("/override/"));
            assert_eq!(body["actions"], json!([]));
            assert_eq!(body["conditions"][0]["pattern"], id.as_str());
        }
        sync_rules(&server, &client, true).await;
        assert!(
            summaries(&client)
                .await
                .into_iter()
                .find(|room| room.id == id)
                .unwrap()
                .muted
        );
        set_room_muted(&client, &id, false).await.unwrap();
        assert!(
            server
                .traffic
                .lock()
                .unwrap()
                .push_requests
                .iter()
                .any(|(method, path, _)| method == "DELETE" && path.contains("/override/"))
        );
        sync_rules(&server, &client, false).await;
        assert!(
            !summaries(&client)
                .await
                .into_iter()
                .find(|room| room.id == id)
                .unwrap()
                .muted
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn rejected_mute_reports_failure_without_changing_room_state() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = room_admin_tests::setup(false).await;
        server.traffic.lock().unwrap().reject_push = true;
        let (outbox, mut updates) = ui::unbounded();
        handle(
            &client,
            &outbox,
            &Arc::new(Shared::default()),
            Command::MuteRoom {
                room: id.clone(),
                muted: true,
            },
        )
        .await
        .unwrap();
        let Some(Update::RoomMuted { room, result, .. }) = updates.next().await else {
            panic!("Expected mute result");
        };
        assert_eq!(room, id);
        assert!(result.is_err());
        assert!(
            !summaries(&client)
                .await
                .into_iter()
                .find(|room| room.id == id)
                .unwrap()
                .muted
        );
        assert!(
            set_room_muted(&client, &"!missing:example.org".parse().unwrap(), true)
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
}
