use super::*;
use matrix_sdk::ruma::events::receipt::{ReceiptEventContent, ReceiptThread, SyncReceiptEvent};
use serde_json::json;

#[tokio::test]
async fn public_receipts_arrive_live_and_reload_from_the_sdk_store_without_private_receipts() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (server, client, id) = room_admin_tests::setup(false).await;
        let (outbox, mut updates) = ui::unbounded();
        client.add_event_handler(move |event: SyncReceiptEvent, room: Room| {
            let outbox = outbox.clone();
            async move {
                let readers = receipts::live(&room, event.content).await;
                post(&outbox, Update::Receipts { room: room.room_id().to_owned(), readers });
            }
        });
        let content = json!({"$one":{
            "m.read":{"@member:example.org":{"ts":100},"@tester:example.org":{"ts":101}},
            "m.read.private":{"@private:example.org":{"ts":102}}
        }});
        server.traffic.lock().unwrap().sync = Some(json!({"next_batch":"receipts", "rooms":{"join":{id.as_str():{
            "timeline":{"events":[],"limited":false},
            "ephemeral":{"events":[{"type":"m.receipt", "content":content}]}
        }}}}));
        client.sync_once(SyncSettings::default()).await.unwrap();
        let Some(Update::Receipts { room: received_room, readers }) = updates.next().await else { panic!("Expected live receipts"); };
        assert_eq!(received_room, id);
        assert_eq!(readers.len(), 1);
        assert_eq!(readers[0].user.as_str(), "@member:example.org");
        assert_eq!(readers[0].name, "Member");
        assert_eq!(readers[0].timestamp, Some(100));
        let room = client.get_room(&id).unwrap();
        let raw = Raw::from_json_string(json!({"type":"m.room.message","event_id":"$one","sender":"@tester:example.org","origin_server_ts":1,"content":{"msgtype":"m.text","body":"Hi"}}).to_string()).unwrap();
        let events = [parse(&room, &raw).await.unwrap()];
        let cached = receipts::snapshot(&room, &events, None).await;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].user.as_str(), "@member:example.org");
        assert_eq!(cached[0].event.as_str(), "$one");
        // Older clients omit thread_id, while newer clients send separate thread markers.
        let threaded: ReceiptEventContent = serde_json::from_value(json!({"$reply":{
            "m.read":{"@member:example.org":{"ts":200,"thread_id":"$root"}},
            "m.read.private":{"@private:example.org":{"ts":201,"thread_id":"$root"}}
        }})).unwrap();
        let threaded = receipts::live(&room, threaded).await;
        assert_eq!(threaded.len(), 1);
        assert_eq!(threaded[0].thread, ReceiptThread::Thread("$root".try_into().unwrap()));
    }).await.expect("Read receipts integration test timed out");
}
