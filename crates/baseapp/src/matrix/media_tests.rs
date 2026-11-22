//! Exercise the real Matrix upload/download API against an in-process homeserver.
use super::*;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, Ordering},
};

#[path = "room_admin_tests.rs"]
mod room_admin_tests;

#[path = "receipt_tests.rs"]
mod receipt_tests;

#[path = "profile_tests.rs"]
mod profile_tests;

#[path = "unread_tests.rs"]
mod unread_tests;

fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            result.push(value);
            index += 3;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(result).unwrap()
}

#[derive(Default)]
struct Traffic {
    presence: Option<serde_json::Value>,
    profile: serde_json::Value,
    capabilities: Option<serde_json::Value>,
    profile_writes: Vec<(String, serde_json::Value)>,
    reject_profile_field: Option<String>,
    room_state: Option<Vec<serde_json::Value>>,
    admin_requests: Vec<(String, serde_json::Value)>,
    directory: String,
    reject_admin: bool,
    aliases: HashMap<String, String>,
    sync: Option<serde_json::Value>,
    state_writes: Vec<(String, serde_json::Value)>,
    uploads: Vec<(String, Vec<u8>)>,
    sent: Vec<serde_json::Value>,
    files: HashMap<String, Vec<u8>>,
    downloads: Vec<String>,
    reject_uploads: bool,
    created_rooms: usize,
    leaves: Vec<String>,
    reject_leaves: bool,
}

#[tokio::test]
async fn room_settings_save_for_admins_and_recheck_revoked_permissions() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = Server::new();
        let client = Client::builder().homeserver_url(&server.url)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
            .build().await.unwrap();
        let session: MatrixSession = serde_json::from_value(serde_json::json!({
            "user_id":"@tester:example.org", "device_id":"SETTINGS", "access_token":"test-token"
        })).unwrap();
        client.restore_session(session).await.unwrap();
        let id: OwnedRoomId = "!settings:example.org".parse().unwrap();
        let sync = |level: i64, token: &str| serde_json::json!({
            "next_batch":token,
            "rooms":{"join":{id.as_str():{
                "state":{"events":[
                    {"type":"m.room.create","state_key":"","event_id":"$create","sender":"@tester:example.org","origin_server_ts":1,"content":{"creator":"@tester:example.org","room_version":"11"}},
                    {"type":"m.room.member","state_key":"@tester:example.org","event_id":"$member","sender":"@tester:example.org","origin_server_ts":2,"content":{"membership":"join"}},
                    {"type":"m.room.power_levels","state_key":"","event_id":format!("$power{token}"),"sender":"@tester:example.org","origin_server_ts":3,"content":{"users":{"@tester:example.org":level},"state_default":50}},
                    {"type":"m.room.name","state_key":"","event_id":"$name","sender":"@tester:example.org","origin_server_ts":4,"content":{"name":"Old room"}}
                ]},
                "timeline":{"events":[],"limited":false,"prev_batch":"history"}
            }}}
        });
        server.traffic.lock().unwrap().sync = Some(sync(100, "admin"));
        client.sync_once(SyncSettings::default()).await.unwrap();
        let summary = summaries(&client).await.into_iter().find(|room| room.id == id).unwrap();
        assert!(summary.can_edit_name && summary.can_edit_topic);
        assert_eq!(summary.explicit_name.as_deref(), Some("Old room"));
        let (outbox, mut updates) = ui::unbounded();
        let shared = Arc::new(Shared::default());
        handle(&client, &outbox, &shared, Command::RoomSettings {
            room: id.clone(), name: Some("New room".into()), topic: Some("Room description".into()),
        }).await.unwrap();
        assert!(matches!(updates.next().await, Some(Update::RoomSettingsSaved { error: None, .. })));
        {
            let traffic = server.traffic.lock().unwrap();
            assert_eq!(traffic.state_writes.len(), 2);
            assert!(traffic.state_writes[0].0.contains("/m.room.name"));
            assert_eq!(traffic.state_writes[0].1["name"], "New room");
            assert!(traffic.state_writes[1].0.contains("/m.room.topic"));
            assert_eq!(traffic.state_writes[1].1["topic"], "Room description");
        }
        // The dialog may already be open when an administrator removes this role.
        server.traffic.lock().unwrap().sync = Some(sync(0, "revoked"));
        client.sync_once(SyncSettings::default()).await.unwrap();
        let summary = summaries(&client).await.into_iter().find(|room| room.id == id).unwrap();
        assert!(!summary.can_edit_name && !summary.can_edit_topic);
        handle(&client, &outbox, &shared, Command::RoomSettings {
            room: id, name: Some("Denied".into()), topic: None,
        }).await.unwrap();
        assert!(matches!(updates.next().await, Some(Update::Rooms(_))));
        assert!(matches!(updates.next().await, Some(Update::RoomSettingsSaved { error: Some(_), .. })));
        assert_eq!(server.traffic.lock().unwrap().state_writes.len(), 2);
    }).await.expect("Room settings integration test timed out");
}

#[tokio::test]
async fn leaving_a_space_keeps_child_rooms_joined_and_preserves_membership_on_failure() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = Server::new();
        let client = Client::builder()
            .homeserver_url(&server.url)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
            .build()
            .await
            .unwrap();
        let session: MatrixSession = serde_json::from_value(serde_json::json!({
            "user_id":"@tester:example.org", "device_id":"SPACE", "access_token":"test-token"
        }))
        .unwrap();
        client.restore_session(session).await.unwrap();
        let mut request = create_room::v3::Request::new();
        request.creation_content =
            Some(serde_json::from_value(serde_json::json!({"type":"m.space"})).unwrap());
        let space = client.create_room(request).await.unwrap();
        let child = client
            .create_room(create_room::v3::Request::new())
            .await
            .unwrap();
        let (outbox, mut updates) = ui::unbounded();
        let shared = Arc::new(Shared::default());
        handle(
            &client,
            &outbox,
            &shared,
            Command::Leave(space.room_id().to_owned()),
        )
        .await
        .unwrap();
        assert_eq!(space.state(), RoomState::Left);
        assert_eq!(child.state(), RoomState::Joined);
        match updates.next().await.unwrap() {
            Update::Rooms(rooms) => {
                assert!(!rooms.iter().any(|room| room.id == space.room_id()));
                assert!(rooms.iter().any(|room| room.id == child.room_id()));
            }
            _ => panic!("Expected updated room list after leaving the space"),
        }
        assert_eq!(server.traffic.lock().unwrap().leaves.len(), 1);
        let another = client
            .create_room(create_room::v3::Request::new())
            .await
            .unwrap();
        server.traffic.lock().unwrap().reject_leaves = true;
        assert!(
            handle(
                &client,
                &outbox,
                &shared,
                Command::Leave(another.room_id().to_owned())
            )
            .await
            .is_err()
        );
        assert_eq!(another.state(), RoomState::Joined);
        assert_eq!(child.state(), RoomState::Joined);
    })
    .await
    .expect("Space removal test timed out");
}

#[tokio::test]
async fn cache_clear_removes_downloads_but_preserves_session_and_pending_uploads() {
    use matrix_sdk::media::store::IgnoreMediaRetentionPolicy;
    let server = Server::new();
    let client = Client::builder()
        .homeserver_url(&server.url)
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
        .build()
        .await
        .unwrap();
    let session: MatrixSession = serde_json::from_value(serde_json::json!({
        "user_id":"@tester:example.org", "device_id":"CACHE", "access_token":"test-token"
    }))
    .unwrap();
    client.restore_session(session).await.unwrap();
    let room = client
        .create_room(create_room::v3::Request::new())
        .await
        .unwrap();
    let policy = cache_policy();
    client
        .media()
        .set_media_retention_policy(policy)
        .await
        .unwrap();
    let downloaded = MediaRequestParameters {
        source: MediaSource::Plain("mxc://example.org/downloaded".into()),
        format: MediaFormat::File,
    };
    let pending = MediaRequestParameters {
        source: MediaSource::Plain("mxc://example.org/pending".into()),
        format: MediaFormat::File,
    };
    let poster = video_preview_request(&downloaded.source);
    {
        let store = client.media_store().lock().await.unwrap();
        store
            .add_media_content(&downloaded, vec![1, 2, 3], IgnoreMediaRetentionPolicy::No)
            .await
            .unwrap();
        store
            .add_media_content(&poster, vec![4, 5, 6], IgnoreMediaRetentionPolicy::No)
            .await
            .unwrap();
        store
            .add_media_content(&pending, vec![7, 8, 9], IgnoreMediaRetentionPolicy::Yes)
            .await
            .unwrap();
    }
    clear_media_cache(&client, &Shared::default())
        .await
        .unwrap();
    let store = client.media_store().lock().await.unwrap();
    assert!(
        store
            .get_media_content(&downloaded)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.get_media_content(&poster).await.unwrap().is_none());
    assert_eq!(
        store.get_media_content(&pending).await.unwrap(),
        Some(vec![7, 8, 9])
    );
    assert_eq!(store.media_retention_policy(), policy);
    assert_eq!(client.user_id().unwrap().as_str(), "@tester:example.org");
    assert!(client.get_room(room.room_id()).is_some());
}

struct Server {
    url: String,
    traffic: Arc<Mutex<Traffic>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = traffic.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                let header_end = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break None;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                        break Some(end + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let header = String::from_utf8_lossy(&request[..header_end]).to_string();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let path = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let mut traffic = captured.lock().unwrap();
                let mut status = "200 OK";
                let mut content_type = "application/json";
                let response = if path.contains("/capabilities") {
                    serde_json::to_vec(&serde_json::json!({"capabilities":traffic.capabilities.clone().unwrap_or_else(|| serde_json::json!({}))})).unwrap()
                } else if path.contains("/presence/") {
                    if let Some(presence) = &traffic.presence {
                        serde_json::to_vec(presence).unwrap()
                    } else {
                        status = "403 Forbidden";
                        br#"{"errcode":"M_FORBIDDEN","error":"Presence not shared"}"#.to_vec()
                    }
                } else if let Some((_, suffix)) = path.split_once("/profile/") {
                    let field = suffix
                        .split_once('/')
                        .map(|(_, field)| decode_component(field.split('?').next().unwrap()));
                    if let Some(field) = field {
                        if header.starts_with("GET ") {
                            let mut body = serde_json::Map::new();
                            if let Some(value) = traffic.profile.get(&field) {
                                body.insert(field.clone(), value.clone());
                            }
                            serde_json::to_vec(&body).unwrap()
                        } else if traffic.reject_profile_field.as_ref() == Some(&field) {
                            status = "403 Forbidden";
                            br#"{"errcode":"M_FORBIDDEN","error":"Profile field disabled"}"#
                                .to_vec()
                        } else {
                            if !traffic.profile.is_object() {
                                traffic.profile = serde_json::json!({});
                            }
                            if header.starts_with("DELETE ") {
                                traffic.profile.as_object_mut().unwrap().remove(&field);
                                traffic
                                    .profile_writes
                                    .push((field, serde_json::Value::Null));
                            } else {
                                let body: serde_json::Value = serde_json::from_slice(
                                    &request[header_end..header_end + length],
                                )
                                .unwrap();
                                traffic.profile[&field] = body[&field].clone();
                                traffic.profile_writes.push((field, body));
                            }
                            b"{}".to_vec()
                        }
                    } else {
                        if traffic.profile.is_object() {
                            serde_json::to_vec(&traffic.profile).unwrap()
                        } else {
                            b"{}".to_vec()
                        }
                    }
                } else if path.contains("/keys/upload") {
                    br#"{"one_time_key_counts":{"signed_curve25519":50}}"#.to_vec()
                } else if let Some((_, alias)) = path.split_once("/directory/room/") {
                    let alias = decode_component(alias);
                    if header.starts_with("PUT ") {
                        let body: serde_json::Value =
                            serde_json::from_slice(&request[header_end..header_end + length])
                                .unwrap();
                        traffic
                            .aliases
                            .insert(alias, body["room_id"].as_str().unwrap().to_owned());
                        b"{}".to_vec()
                    } else if header.starts_with("DELETE ") {
                        traffic.aliases.remove(&alias);
                        b"{}".to_vec()
                    } else if let Some(id) = traffic.aliases.get(&alias) {
                        serde_json::to_vec(
                            &serde_json::json!({"room_id":id,"servers":["example.org"]}),
                        )
                        .unwrap()
                    } else {
                        status = "404 Not Found";
                        br#"{"errcode":"M_NOT_FOUND","error":"No alias"}"#.to_vec()
                    }
                } else if path.contains("/directory/list/room/") {
                    if header.starts_with("PUT ") {
                        let body: serde_json::Value =
                            serde_json::from_slice(&request[header_end..header_end + length])
                                .unwrap();
                        traffic.directory = body["visibility"].as_str().unwrap().to_owned();
                        b"{}".to_vec()
                    } else {
                        serde_json::to_vec(&serde_json::json!({"visibility":if traffic.directory.is_empty() { "private" } else { &traffic.directory }})).unwrap()
                    }
                } else if ["/invite", "/kick", "/ban", "/unban"]
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
                {
                    traffic.admin_requests.push((
                        path.to_owned(),
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap(),
                    ));
                    if traffic.reject_admin {
                        status = "403 Forbidden";
                        br#"{"errcode":"M_FORBIDDEN","error":"Denied"}"#.to_vec()
                    } else {
                        b"{}".to_vec()
                    }
                } else if path.contains("/sync") {
                    let sync = traffic
                        .sync
                        .take()
                        .unwrap_or_else(|| serde_json::json!({"next_batch":"empty"}));
                    if let Some(rooms) = sync["rooms"]["join"].as_object()
                        && let Some(events) = rooms
                            .values()
                            .find_map(|room| room["state"]["events"].as_array())
                    {
                        traffic.room_state = Some(events.clone());
                    }
                    serde_json::to_vec(&sync).unwrap()
                } else if path.contains("/state/") && header.starts_with("PUT ") {
                    let suffix = path.split_once("/state/").unwrap().1;
                    let (kind, key) = suffix.split_once('/').unwrap_or((suffix, ""));
                    let kind = decode_component(kind);
                    let key = decode_component(key);
                    let content: serde_json::Value =
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                    if let Some(state) = &mut traffic.room_state {
                        state.retain(|event| event["type"] != kind || event["state_key"] != key);
                        state.push(serde_json::json!({"type":kind,"state_key":key,"content":content,"event_id":"$updated","sender":"@tester:example.org","origin_server_ts":10}));
                    }
                    traffic.state_writes.push((
                        path.to_owned(),
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap(),
                    ));
                    serde_json::to_vec(&serde_json::json!({"event_id":format!("$state{}", traffic.state_writes.len())})).unwrap()
                } else if path.contains("/upload") && traffic.reject_uploads {
                    status = "403 Forbidden";
                    br#"{"errcode":"M_FORBIDDEN","error":"Uploads disabled"}"#.to_vec()
                } else if path.contains("/upload") {
                    let id = format!("file{}", traffic.uploads.len());
                    let mime = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                                .map(|(_, value)| value.trim().to_owned())
                        })
                        .unwrap();
                    let body = request[header_end..header_end + length].to_vec();
                    traffic.files.insert(id.clone(), body.clone());
                    traffic.uploads.push((mime, body));
                    serde_json::to_vec(
                        &serde_json::json!({"content_uri":format!("mxc://example.org/{id}")}),
                    )
                    .unwrap()
                } else if path.contains("/download/") {
                    let id = path.split('?').next().unwrap().rsplit('/').next().unwrap();
                    traffic.downloads.push(id.to_owned());
                    match traffic.files.get(id) {
                        Some(bytes) => {
                            content_type = "application/octet-stream";
                            bytes.clone()
                        }
                        None => {
                            status = "404 Not Found";
                            br#"{"errcode":"M_NOT_FOUND","error":"Missing"}"#.to_vec()
                        }
                    }
                } else if path.contains("/send/m.room.message/") {
                    let body =
                        serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                    traffic.sent.push(body);
                    br#"{"event_id":"$sent:example.org"}"#.to_vec()
                } else if path.contains("/config") {
                    br#"{"m.upload.size":20971520}"#.to_vec()
                } else if path.contains("/createRoom") {
                    let id = if traffic.created_rooms == 0 {
                        "!media:example.org".to_owned()
                    } else {
                        format!("!room-{}:example.org", traffic.created_rooms)
                    };
                    traffic.created_rooms += 1;
                    serde_json::to_vec(&serde_json::json!({"room_id":id})).unwrap()
                } else if path.split('?').next().unwrap().ends_with("/leave") {
                    traffic.leaves.push(path.to_owned());
                    if traffic.reject_leaves {
                        status = "400 Bad Request";
                        br#"{"errcode":"M_UNKNOWN","error":"Leaving unavailable"}"#.to_vec()
                    } else {
                        b"{}".to_vec()
                    }
                } else if path.contains("/state/") {
                    let suffix = path.split_once("/state/").unwrap().1;
                    let (kind, key) = suffix.split_once('/').unwrap_or((suffix, ""));
                    let kind = decode_component(kind);
                    let key = decode_component(key.split('?').next().unwrap());
                    if let Some(event) = traffic.room_state.as_ref().and_then(|state| {
                        state
                            .iter()
                            .find(|event| event["type"] == kind && event["state_key"] == key)
                    }) {
                        serde_json::to_vec(&event["content"]).unwrap()
                    } else {
                        status = "404 Not Found";
                        br#"{"errcode":"M_NOT_FOUND","error":"No state"}"#.to_vec()
                    }
                } else if path.contains("/state") {
                    serde_json::to_vec(&traffic.room_state.as_deref().unwrap_or_default()).unwrap()
                } else if path.contains("/members") {
                    br#"{"chunk":[]}"#.to_vec()
                } else {
                    b"{}".to_vec()
                };
                drop(traffic);
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                stream.write_all(&response).ok();
            }
        });
        Self {
            url,
            traffic,
            stop,
            thread: Some(thread),
        }
    }
}

#[tokio::test]
async fn uploads_video_thumbnail_and_downloads_preview_and_original() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let server = Server::new();
        let client = Client::builder()
            .homeserver_url(&server.url)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
            .build()
            .await
            .unwrap();
        let session: MatrixSession = serde_json::from_value(serde_json::json!({
            "user_id":"@tester:example.org", "device_id":"MEDIA", "access_token":"test-token"
        }))
        .unwrap();
        client.restore_session(session).await.unwrap();
        let room = client
            .create_room(create_room::v3::Request::new())
            .await
            .unwrap();
        let bytes = include_bytes!("../../tests/media/clip.mp4").to_vec();
        let info = super::super::playback::inspect(&bytes, "video/mp4").unwrap();
        let expected_poster = info.thumbnail.as_ref().unwrap().bytes.clone();
        let (outbox, mut updates) = ui::unbounded();
        let shared = Arc::new(Shared::default());
        handle(
            &client,
            &outbox,
            &shared,
            Command::Picture {
                room: room.room_id().to_owned(),
                name: "clip.mp4".into(),
                mime: "video/mp4".into(),
                bytes: bytes.clone(),
                width: info.width,
                height: info.height,
                caption: "A video caption".into(),
                duration: info.duration,
                thumbnail: info.thumbnail,
                local: 1,
                reply: None,
                thread: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            updates.next().await,
            Some(Update::Sent { event: Some(_), .. })
        ));
        let sent = {
            let traffic = server.traffic.lock().unwrap();
            assert_eq!(traffic.uploads.len(), 2);
            assert!(
                traffic
                    .uploads
                    .iter()
                    .any(|(mime, body)| mime == "video/mp4" && body == &bytes)
            );
            assert!(
                traffic
                    .uploads
                    .iter()
                    .any(|(mime, body)| mime == "image/jpeg" && body == &expected_poster)
            );
            traffic.sent.last().unwrap().clone()
        };
        assert_eq!(sent["msgtype"], "m.video");
        assert_eq!(sent["info"]["w"], 64);
        assert_eq!(sent["info"]["h"], 48);
        assert_eq!(sent["info"]["thumbnail_info"]["mimetype"], "image/jpeg");
        let message: MessageType = serde_json::from_value(sent).unwrap();
        let (_, caption, _, picture) = describe(&message);
        assert!(!caption.is_empty());
        let picture = picture.unwrap();
        handle(
            &client,
            &outbox,
            &shared,
            Command::VideoPreview {
                key: picture.key.clone(),
                source: picture.source.clone(),
                thumbnail: picture.thumbnail,
            },
        )
        .await
        .unwrap();
        match updates.next().await.unwrap() {
            Update::VideoPreview {
                thumbnail: Some(poster),
                bytes: None,
                ..
            } => assert_eq!(poster, expected_poster),
            _ => panic!("Expected advertised preview without downloading the video"),
        }
        client
            .media()
            .remove_media_content(&video_preview_request(&picture.source))
            .await
            .unwrap();
        handle(
            &client,
            &outbox,
            &shared,
            Command::VideoPreview {
                key: picture.key.clone(),
                source: picture.source.clone(),
                thumbnail: Some(MediaSource::Plain("mxc://example.org/missing".into())),
            },
        )
        .await
        .unwrap();
        match updates.next().await.unwrap() {
            Update::VideoPreview {
                thumbnail: Some(poster),
                bytes: None,
                ..
            } => {
                assert_eq!(poster, expected_poster);
            }
            _ => panic!("Expected native fallback preview without retaining the video in RAM"),
        }
        let downloads = server.traffic.lock().unwrap().downloads.len();
        handle(
            &client,
            &outbox,
            &shared,
            Command::VideoPreview {
                key: picture.key.clone(),
                source: picture.source.clone(),
                thumbnail: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            updates.next().await,
            Some(Update::VideoPreview {
                thumbnail: Some(_),
                bytes: None,
                ..
            })
        ));
        assert_eq!(
            server.traffic.lock().unwrap().downloads.len(),
            downloads,
            "Cached video previews must not download the original again"
        );
        handle(
            &client,
            &outbox,
            &shared,
            Command::Fetch {
                key: picture.key,
                source: picture.source,
                size: None,
            },
        )
        .await
        .unwrap();
        match updates.next().await.unwrap() {
            Update::Media {
                bytes: Some(original),
                ..
            } => assert_eq!(original, bytes),
            _ => panic!("Expected original playable bytes"),
        }
        assert_eq!(
            server.traffic.lock().unwrap().downloads.len(),
            downloads,
            "Opening a video after its preview must reuse the SDK cache"
        );
        // Every advertised container/extension uses the same real SDK upload path.
        for (name, data) in [
            (
                "clip.m4v",
                include_bytes!("../../tests/media/clip.m4v").as_slice(),
            ),
            (
                "clip.mov",
                include_bytes!("../../tests/media/clip.mov").as_slice(),
            ),
            (
                "clip.mkv",
                include_bytes!("../../tests/media/clip.mkv").as_slice(),
            ),
            (
                "clip.webm",
                include_bytes!("../../tests/media/clip.webm").as_slice(),
            ),
            (
                "tone.mp3",
                include_bytes!("../../tests/media/tone.mp3").as_slice(),
            ),
            (
                "tone.m4a",
                include_bytes!("../../tests/media/tone.m4a").as_slice(),
            ),
            (
                "tone.aac",
                include_bytes!("../../tests/media/tone.aac").as_slice(),
            ),
            (
                "tone.wav",
                include_bytes!("../../tests/media/tone.wav").as_slice(),
            ),
            (
                "tone.flac",
                include_bytes!("../../tests/media/tone.flac").as_slice(),
            ),
            (
                "tone.ogg",
                include_bytes!("../../tests/media/tone.ogg").as_slice(),
            ),
            (
                "tone.oga",
                include_bytes!("../../tests/media/tone.ogg").as_slice(),
            ),
        ] {
            let mime = super::super::playback::mime(name).unwrap();
            let info = super::super::playback::inspect(data, mime).unwrap();
            handle(
                &client,
                &outbox,
                &shared,
                Command::Picture {
                    room: room.room_id().to_owned(),
                    name: name.into(),
                    mime: mime.into(),
                    bytes: data.to_vec(),
                    width: info.width,
                    height: info.height,
                    duration: info.duration,
                    thumbnail: info.thumbnail,
                    caption: String::new(),
                    local: 2,
                    reply: None,
                    thread: None,
                },
            )
            .await
            .unwrap();
            assert!(
                matches!(
                    updates.next().await,
                    Some(Update::Sent { event: Some(_), .. })
                ),
                "{name}"
            );
            let traffic = server.traffic.lock().unwrap();
            let sent = traffic.sent.last().unwrap();
            assert_eq!(
                sent["msgtype"],
                if mime.starts_with("video/") {
                    "m.video"
                } else {
                    "m.audio"
                },
                "{name}"
            );
            assert_eq!(sent["info"]["mimetype"], mime, "{name}");
            assert_eq!(sent["info"]["size"], data.len(), "{name}");
            assert!(
                traffic
                    .uploads
                    .iter()
                    .any(|(uploaded_mime, bytes)| uploaded_mime == mime && bytes == data),
                "{name}"
            );
        }
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::Gif, "image/gif"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Bmp, "image/bmp"),
        ] {
            let mut data = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(16, 12)
                .write_to(&mut data, format)
                .unwrap();
            let bytes = data.into_inner();
            assert!(super::super::sniff(&bytes).is_some());
            assert_eq!(super::super::measure(&bytes), (16, 12));
            handle(
                &client,
                &outbox,
                &shared,
                Command::Picture {
                    room: room.room_id().to_owned(),
                    name: format!("still.{}", format.extensions_str()[0]),
                    mime: mime.into(),
                    bytes: bytes.clone(),
                    width: 16,
                    height: 12,
                    duration: None,
                    thumbnail: None,
                    caption: String::new(),
                    local: 3,
                    reply: None,
                    thread: None,
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                updates.next().await,
                Some(Update::Sent { event: Some(_), .. })
            ));
            let traffic = server.traffic.lock().unwrap();
            let sent = traffic.sent.last().unwrap();
            assert_eq!(sent["msgtype"], "m.image");
            assert_eq!(sent["info"]["mimetype"], mime);
        }
        server.traffic.lock().unwrap().reject_uploads = true;
        let failed = handle(
            &client,
            &outbox,
            &shared,
            Command::Picture {
                room: room.room_id().to_owned(),
                name: "failed.mp4".into(),
                mime: "video/mp4".into(),
                bytes,
                width: 64,
                height: 48,
                duration: None,
                thumbnail: None,
                caption: String::new(),
                local: 99,
                reply: None,
                thread: None,
            },
        )
        .await;
        assert!(failed.is_err());
        assert!(matches!(
            updates.next().await,
            Some(Update::Sent {
                local: 99,
                event: None,
                ..
            })
        ));
        handle(
            &client,
            &outbox,
            &shared,
            Command::VideoPreview {
                key: "missing-preview".into(),
                source: MediaSource::Plain("mxc://example.org/missing-video".into()),
                thumbnail: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            updates.next().await,
            Some(Update::VideoPreview {
                bytes: None,
                thumbnail: None,
                ..
            })
        ));
    })
    .await
    .expect("Media API integration test timed out");
}
