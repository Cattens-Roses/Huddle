use super::super::super::profile::{self, AvatarEdit, Edit, KEYS};
use super::*;
use serde_json::json;

async fn setup(capabilities: serde_json::Value) -> (Server, Client) {
    let server = Server::new();
    server.traffic.lock().unwrap().profile = json!({"displayname":"Tester"});
    server.traffic.lock().unwrap().capabilities = Some(capabilities);
    let client = Client::builder()
        .homeserver_url(&server.url)
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_16])
        .build()
        .await
        .unwrap();
    client.restore_session(serde_json::from_value::<MatrixSession>(json!({"user_id":"@tester:example.org", "device_id":"PROFILE", "access_token":"test-token"})).unwrap()).await.unwrap();
    (server, client)
}
fn unchanged() -> Edit {
    Edit {
        name: None,
        avatar: AvatarEdit::Keep,
        fields: Default::default(),
        room: None,
    }
}

#[tokio::test]
async fn profile_fields_save_reload_and_delete_without_overwriting_global_name_or_photo() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client) = setup(json!({"m.profile_fields":{"enabled":true}})).await;
        let loaded = profile::load(&client, None).await.unwrap();
        assert!(loaded.allowed.into_iter().all(|allowed| allowed));
        let mut edit = unchanged();
        edit.fields = [
            Some("About me".into()),
            Some("they/them".into()),
            Some("https://example.org".into()),
            Some("Europe/Warsaw".into()),
        ];
        let saved = profile::save(&client, edit).await.unwrap();
        assert_eq!(saved.public.values[0], "About me");
        assert!(!saved.changed_name && !saved.changed_avatar);
        assert_eq!(saved.name, "Tester");
        assert_eq!(server.traffic.lock().unwrap().profile_writes.len(), 4);
        assert!(
            server
                .traffic
                .lock()
                .unwrap()
                .profile_writes
                .iter()
                .all(|(field, _)| field != "displayname" && field != "avatar_url")
        );
        let loaded = profile::load(&client, None).await.unwrap();
        assert_eq!(loaded.public.values[1], "they/them");
        let mut edit = unchanged();
        edit.fields[0] = Some(String::new());
        profile::save(&client, edit).await.unwrap();
        assert!(profile::load(&client, None).await.unwrap().public.values[0].is_empty());
        assert!(
            server
                .traffic
                .lock()
                .unwrap()
                .profile
                .get(KEYS[0])
                .is_none()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn restricted_and_legacy_servers_reject_extra_fields_before_any_profile_write() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client) = setup(json!({"m.profile_fields":{"enabled":true,"allowed":["displayname","avatar_url",KEYS[0]]}})).await;
        assert_eq!(profile::load(&client, None).await.unwrap().allowed, [true, false, false, false]);
        let mut edit = unchanged(); edit.name = Some("New name".into()); edit.fields[1] = Some("she/her".into());
        assert!(profile::save(&client, edit).await.is_err());
        assert!(server.traffic.lock().unwrap().profile_writes.is_empty());
        let (legacy_server, legacy, _) = room_admin_tests::setup(false).await;
        legacy_server.traffic.lock().unwrap().profile = json!({"displayname":"Tester"});
        let loaded = profile::load(&legacy, None).await.unwrap();
        assert_eq!(loaded.allowed, [false; 4]);
        assert!(loaded.can_name && loaded.can_avatar);
    }).await.unwrap();
}

#[tokio::test]
async fn room_nicknames_are_saved_only_in_the_selected_room() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, room) = room_admin_tests::setup(false).await;
        server.traffic.lock().unwrap().profile = json!({"displayname":"Global name"});
        let loaded = profile::load(&client, Some(&room)).await.unwrap();
        assert!(loaded.room.is_some());
        let mut edit = unchanged();
        edit.room = Some((room.clone(), "Room nickname".into()));
        let saved = profile::save(&client, edit).await.unwrap();
        assert_eq!(saved.name, "Global name");
        assert_eq!(saved.room.unwrap().1, "Room nickname");
        let traffic = server.traffic.lock().unwrap();
        assert!(traffic.profile_writes.is_empty());
        assert_eq!(traffic.state_writes.len(), 1);
        assert!(traffic.state_writes[0].0.contains("/m.room.member/"));
        assert_eq!(traffic.state_writes[0].1["membership"], "join");
        assert_eq!(traffic.state_writes[0].1["displayname"], "Room nickname");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_websites_are_rejected_before_writing_and_partial_errors_are_reported() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client) = setup(json!({"m.profile_fields":{"enabled":true}})).await;
        let mut edit = unchanged();
        edit.fields[0] = Some("Bio".into());
        edit.fields[2] = Some("javascript:alert(1)".into());
        assert!(profile::save(&client, edit).await.is_err());
        assert!(server.traffic.lock().unwrap().profile_writes.is_empty());
        server.traffic.lock().unwrap().reject_profile_field = Some(KEYS[1].into());
        let mut edit = unchanged();
        edit.fields[0] = Some("Bio".into());
        edit.fields[1] = Some("he/him".into());
        let error = profile::save(&client, edit)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.starts_with("Some profile changes may have been saved."));
        assert_eq!(server.traffic.lock().unwrap().profile[KEYS[0]], "Bio");
        assert_eq!(
            server.traffic.lock().unwrap().profile["displayname"],
            "Tester"
        );
    })
    .await
    .unwrap();
}

#[test]
fn unicode_lengths_and_public_website_validation() {
    let mut fields: [Option<String>; 4] = Default::default();
    fields[0] = Some("ą".repeat(1000));
    assert!(profile::validate(&fields, Some("Name"), None).is_ok());
    fields[0] = Some("ą".repeat(1001));
    assert!(profile::validate(&fields, None, None).is_err());
    fields[0] = None;
    fields[2] = Some("https://user:password@example.org".into());
    assert!(profile::validate(&fields, None, None).is_err());
    fields[2] = Some("https://example.org/about".into());
    assert!(profile::validate(&fields, None, None).is_ok());
}
