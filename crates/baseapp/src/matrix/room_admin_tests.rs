use super::*;
use admin::{Action, MemberAction, Setting};
use serde_json::{Value, json};

async fn setup(space: bool) -> (Server, Client, OwnedRoomId) {
    let server = Server::new();
    let client = Client::builder()
        .homeserver_url(&server.url)
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
        .build()
        .await
        .unwrap();
    let session: MatrixSession = serde_json::from_value(
        json!({"user_id":"@tester:example.org", "device_id":"ADMIN", "access_token":"test-token"}),
    )
    .unwrap();
    client.restore_session(session).await.unwrap();
    let id: OwnedRoomId = "!admin:example.org".parse().unwrap();
    let event = |kind: &str, key: &str, content: Value| json!({"type":kind,"state_key":key,"content":content,"event_id":format!("${kind}{key}"),"sender":"@tester:example.org","origin_server_ts":1});
    let mut create = json!({"creator":"@tester:example.org","room_version":"11"});
    if space {
        create["type"] = json!("m.space");
    }
    let state = vec![
        event("m.room.create", "", create),
        event(
            "m.room.member",
            "@tester:example.org",
            json!({"membership":"join","displayname":"Admin"}),
        ),
        event(
            "m.room.member",
            "@member:example.org",
            json!({"membership":"join","displayname":"Member"}),
        ),
        event(
            "m.room.power_levels",
            "",
            json!({"users":{"@tester:example.org":100,"@member:example.org":0},"state_default":50,"events":{"m.room.power_levels":100,"custom.permission":75}}),
        ),
        event("m.room.name", "", json!({"name":"Admin room"})),
        event("m.room.join_rules", "", json!({"join_rule":"invite"})),
    ];
    server.traffic.lock().unwrap().room_state = Some(state.clone());
    server.traffic.lock().unwrap().sync = Some(
        json!({"next_batch":"admin", "rooms":{"join":{id.as_str():{"state":{"events":state},"timeline":{"events":[],"limited":false,"prev_batch":"old"}}}}}),
    );
    client.sync_once(SyncSettings::default()).await.unwrap();
    (server, client, id)
}

#[tokio::test]
async fn administration_updates_access_and_preserves_other_power_levels() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(false).await;
        let loaded = admin::load(&client, &id).await.unwrap();
        assert!(loaded.permissions.power && loaded.permissions.avatar && loaded.permissions.join);
        assert_eq!(loaded.members.len(), 2);
        assert_eq!(loaded.join, "invite");
        admin::apply(
            &client,
            &id,
            Action::Join {
                rule: "restricted".into(),
                spaces: vec!["!space:example.org".parse().unwrap()],
            },
        )
        .await
        .unwrap();
        admin::apply(
            &client,
            &id,
            Action::Setting(Setting::History, "joined".into()),
        )
        .await
        .unwrap();
        admin::apply(
            &client,
            &id,
            Action::Setting(Setting::Guest, "forbidden".into()),
        )
        .await
        .unwrap();
        admin::apply(
            &client,
            &id,
            Action::Setting(Setting::Directory, "public".into()),
        )
        .await
        .unwrap();
        let mut values = loaded.powers;
        values[1] = 50;
        admin::apply(&client, &id, Action::Powers(values))
            .await
            .unwrap();
        let loaded = admin::load(&client, &id).await.unwrap();
        assert_eq!(loaded.join, "restricted");
        assert_eq!(loaded.allowed_spaces, "!space:example.org");
        assert_eq!(loaded.history, "joined");
        assert_eq!(loaded.directory.as_deref(), Some("public"));
        let traffic = server.traffic.lock().unwrap();
        let powers = &traffic
            .room_state
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e["type"] == "m.room.power_levels")
            .unwrap()["content"];
        assert_eq!(powers["events_default"], 50);
        assert_eq!(powers["users"]["@tester:example.org"], 100);
        assert_eq!(powers["events"]["custom.permission"], 75);
        assert_eq!(powers["events"]["m.room.power_levels"], 100);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn administration_checks_fresh_permissions_and_target_hierarchy() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(false).await;
        admin::apply(
            &client,
            &id,
            Action::Member {
                user: "@member:example.org".parse().unwrap(),
                action: MemberAction::Power(50),
                reason: String::new(),
            },
        )
        .await
        .unwrap();
        admin::apply(
            &client,
            &id,
            Action::Member {
                user: "@member:example.org".parse().unwrap(),
                action: MemberAction::Kick,
                reason: "Test reason".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            server.traffic.lock().unwrap().admin_requests[0].1["reason"],
            "Test reason"
        );
        assert!(
            admin::apply(
                &client,
                &id,
                Action::Member {
                    user: "@tester:example.org".parse().unwrap(),
                    action: MemberAction::Ban,
                    reason: String::new()
                }
            )
            .await
            .is_err()
        );
        assert!(
            admin::apply(
                &client,
                &id,
                Action::Member {
                    user: "@member:example.org".parse().unwrap(),
                    action: MemberAction::Power(101),
                    reason: String::new()
                }
            )
            .await
            .is_err()
        );
        let writes = server.traffic.lock().unwrap().state_writes.len();
        // Revoke the role on the server without syncing the SDK cache.
        {
            let mut traffic = server.traffic.lock().unwrap();
            let powers = traffic
                .room_state
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|e| e["type"] == "m.room.power_levels")
                .unwrap();
            powers["content"]["users"]["@tester:example.org"] = json!(0);
        }
        assert!(
            admin::apply(
                &client,
                &id,
                Action::Setting(Setting::History, "world_readable".into())
            )
            .await
            .is_err()
        );
        assert!(admin::load(&client, &id).await.is_err());
        assert_eq!(server.traffic.lock().unwrap().state_writes.len(), writes);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn space_links_can_be_added_updated_and_removed_without_leaving_rooms() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(true).await;
        let child: OwnedRoomId = "!child:example.org".parse().unwrap();
        admin::apply(
            &client,
            &id,
            Action::Child {
                id: child.clone(),
                order: "01".into(),
                suggested: true,
                remove: false,
            },
        )
        .await
        .unwrap();
        let loaded = admin::load(&client, &id).await.unwrap();
        assert!(loaded.space);
        assert_eq!(loaded.children.len(), 1);
        assert_eq!(loaded.children[0].id, child);
        assert!(loaded.children[0].suggested);
        admin::apply(
            &client,
            &id,
            Action::Child {
                id: child.clone(),
                order: String::new(),
                suggested: false,
                remove: true,
            },
        )
        .await
        .unwrap();
        assert!(admin::load(&client, &id).await.unwrap().children.is_empty());
        assert!(server.traffic.lock().unwrap().leaves.is_empty());
        admin::apply(&client, &id, Action::CreateRoom("New channel".into()))
            .await
            .unwrap();
        let created = admin::load(&client, &id).await.unwrap();
        assert_eq!(created.children.len(), 1);
        assert!(server.traffic.lock().unwrap().room_state.as_ref().unwrap().iter()
            .any(|event| event["type"] == "m.space.parent" && event["state_key"] == id.as_str()));
        assert!(
            admin::apply(
                &client,
                &id,
                Action::Child {
                    id: id.clone(),
                    order: String::new(),
                    suggested: false,
                    remove: false
                }
            )
            .await
            .is_err()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn moderation_surfaces_server_rejection_and_releases_busy_state() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(false).await;
        server.traffic.lock().unwrap().reject_admin = true;
        let (outbox, mut updates) = ui::unbounded();
        handle(
            &client,
            &outbox,
            &Arc::new(Shared::default()),
            Command::RoomAdmin {
                room: id,
                action: Action::Member {
                    user: "@member:example.org".parse().unwrap(),
                    action: MemberAction::Ban,
                    reason: String::new(),
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(updates.next().await, Some(Update::Rooms(_))));
        assert!(matches!(
            updates.next().await,
            Some(Update::RoomAdminDone { error: Some(_), .. })
        ));
        assert!(matches!(
            updates.next().await,
            Some(Update::RoomAdminLoaded { result: Ok(_), .. })
        ));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn room_addresses_preserve_alternatives_and_cannot_remove_another_rooms_alias() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(false).await;
        for alias in ["#first:example.org", "#second:example.org"] {
            admin::apply(
                &client,
                &id,
                Action::Alias {
                    alias: alias.into(),
                    remove: false,
                },
            )
            .await
            .unwrap();
        }
        let snapshot = admin::load(&client, &id).await.unwrap();
        assert_eq!(snapshot.alias, "#second:example.org");
        assert_eq!(snapshot.aliases, ["#first:example.org"]);
        server
            .traffic
            .lock()
            .unwrap()
            .aliases
            .insert("#other:example.org".into(), "!other:example.org".into());
        let before = server.traffic.lock().unwrap().state_writes.len();
        assert!(
            admin::apply(
                &client,
                &id,
                Action::Alias {
                    alias: "#other:example.org".into(),
                    remove: true
                }
            )
            .await
            .is_err()
        );
        assert_eq!(server.traffic.lock().unwrap().state_writes.len(), before);
        admin::apply(
            &client,
            &id,
            Action::Alias {
                alias: "#second:example.org".into(),
                remove: true,
            },
        )
        .await
        .unwrap();
        assert!(
            !server
                .traffic
                .lock()
                .unwrap()
                .aliases
                .contains_key("#second:example.org")
        );
        let snapshot = admin::load(&client, &id).await.unwrap();
        assert!(snapshot.alias.is_empty());
        assert_eq!(snapshot.aliases, ["#first:example.org"]);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn room_icons_upload_and_clear_without_changing_the_account_avatar() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = setup(false).await;
        let bytes = include_bytes!("../../../../assets/tray/huddle.png").as_slice();
        admin::apply(
            &client,
            &id,
            Action::Avatar {
                bytes: bytes.into(),
                mime: "image/png".into(),
            },
        )
        .await
        .unwrap();
        assert!(admin::load(&client, &id).await.unwrap().avatar.is_some());
        assert_eq!(server.traffic.lock().unwrap().uploads.len(), 1);
        admin::apply(&client, &id, Action::RemoveAvatar)
            .await
            .unwrap();
        assert!(admin::load(&client, &id).await.unwrap().avatar.is_none());
    })
    .await
    .unwrap();
}
