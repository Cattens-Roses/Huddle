use super::*;
use serde_json::json;

#[tokio::test]
async fn unread_counts_include_non_notifying_messages_and_clear_after_read_receipts() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = room_admin_tests::setup(false).await;
        client.event_cache().subscribe().unwrap();
        let room = client.get_room(&id).unwrap();
        let (cache, _handles) = room.event_cache().await.unwrap();
        let (_, mut updates) = cache.subscribe().await.unwrap();
        let message = |id: &str, sender: &str| json!({
            "type":"m.room.message","event_id":id,"sender":sender,"origin_server_ts":10,
            "content":{"msgtype":"m.text","body":"Hello"}
        });
        server.traffic.lock().unwrap().sync = Some(json!({"next_batch":"messages", "rooms":{"join":{id.as_str():{
            "timeline":{"events":[message("$one", "@member:example.org"),message("$two", "@member:example.org")],"limited":false},
            "unread_notifications":{"notification_count":0,"highlight_count":0}
        }}}}));
        client.sync_once(SyncSettings::default()).await.unwrap();
        loop {
            if room.num_unread_messages() == 2 { break; }
            updates.recv().await.unwrap();
        }
        let summary = summaries(&client).await.into_iter().find(|room| room.id == id).unwrap();
        assert_eq!(summary.unread, 2);
        assert_eq!(room.unread_notification_counts().notification_count, 0);
        // Reading on another device also clears the counts used by all badges.
        server.traffic.lock().unwrap().sync = Some(json!({"next_batch":"read", "rooms":{"join":{id.as_str():{
            "timeline":{"events":[],"limited":false},
            "ephemeral":{"events":[{"type":"m.receipt", "content":{"$two":{"m.read":{"@tester:example.org":{"ts":20}}}}}]}
        }}}}));
        client.sync_once(SyncSettings::default()).await.unwrap();
        loop {
            if room.num_unread_messages() == 0 { break; }
            updates.recv().await.unwrap();
        }
        assert_eq!(summaries(&client).await.into_iter().find(|room| room.id == id).unwrap().unread, 0);
    }).await.expect("Unread count integration test timed out");
}
