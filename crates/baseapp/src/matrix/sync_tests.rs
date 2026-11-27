use super::*;
use serde_json::json;

async fn client(server: &Server) -> Client {
    let client = Client::builder()
        .homeserver_url(&server.url)
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
        .request_config(matrix_sdk::config::RequestConfig::new().retry_limit(0))
        .build()
        .await
        .unwrap();
    client
        .restore_session(
            serde_json::from_value::<MatrixSession>(
                json!({"user_id":"@tester:example.org","device_id":"SYNC","access_token":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn sync_resumes_after_a_server_error_without_losing_the_last_token() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = Server::new();
        let client = client(&server).await;
        server.traffic.lock().unwrap().sync = Some(json!({"next_batch":"before-outage"}));
        client.sync_once(SyncSettings::default()).await.unwrap();
        {
            let mut traffic = server.traffic.lock().unwrap();
            traffic.sync_paths.clear();
            traffic.sync_failures = 1;
            traffic.sync = Some(json!({"next_batch":"reconnected"}));
        }
        let (outbox, mut updates) = ui::unbounded();
        let running = client.clone();
        let task = tokio::spawn(async move { sync_updates(&running, &outbox).await });
        assert!(matches!(updates.next().await, Some(Update::Rooms(_))));
        task.abort();
        let _ = task.await;
        let traffic = server.traffic.lock().unwrap();
        assert!(traffic.sync_paths.len() >= 2);
        assert!(
            traffic
                .sync_paths
                .iter()
                .take(2)
                .all(|path| path.contains("since=before-outage"))
        );
        assert_eq!(traffic.sync_failures, 0);
    })
    .await
    .expect("Sync did not reconnect");
}

#[tokio::test]
async fn an_expired_session_stops_sync_instead_of_retrying_forever() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = Server::new();
        let client = client(&server).await;
        server.traffic.lock().unwrap().sync_unknown_token = true;
        let (outbox, _updates) = ui::unbounded();
        let error = sync_updates(&client, &outbox).await.unwrap_err();
        assert!(matches!(
            error.client_api_error_kind(),
            Some(ErrorKind::UnknownToken(_))
        ));
        assert_eq!(server.traffic.lock().unwrap().sync_paths.len(), 1);
    })
    .await
    .unwrap();
}

#[test]
fn reconnect_backoff_is_bounded() {
    assert_eq!(
        (0..7)
            .map(|n| sync_retry_delay(n).as_secs())
            .collect::<Vec<_>>(),
        vec![1, 2, 4, 8, 16, 32, 32]
    );
}
