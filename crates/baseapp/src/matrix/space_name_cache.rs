//! Account-scoped discovery metadata, persisted in the Matrix state store.
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

const TTL: u64 = 300;
const MAX_BYTES: usize = 512 * 1024;
const MAX_PARENTS: usize = 64;
static WRITER: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    entries: BTreeMap<OwnedRoomId, Entry>,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    checked: u64,
    children: Vec<OwnedRoomId>,
    names: Vec<(OwnedRoomId, String)>,
}
pub(super) struct Snapshot {
    pub names: Vec<(OwnedRoomId, String)>,
    pub fresh: bool,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn key(client: &Client) -> Vec<u8> {
    format!(
        "huddle.space-names.v1.{}",
        client.user_id().map_or("", UserId::as_str)
    )
    .into_bytes()
}
async fn read(client: &Client) -> Cache {
    match client.state_store().get_custom_value(&key(client)).await {
        Ok(Some(bytes)) if bytes.len() <= MAX_BYTES => {
            serde_json::from_slice(&bytes).unwrap_or_default()
        }
        _ => Cache::default(),
    }
}
pub(super) async fn restore(client: &Client) -> Vec<(OwnedRoomId, String)> {
    let mut entries: Vec<_> = read(client).await.entries.into_values().collect();
    entries.sort_by_key(|entry| entry.checked);
    let mut names = HashMap::new();
    for entry in entries {
        names.extend(entry.names);
    }
    names.into_iter().collect()
}
pub(super) async fn get(
    client: &Client,
    parent: &OwnedRoomId,
    children: &[OwnedRoomId],
) -> Snapshot {
    let entry = read(client).await.entries.remove(parent);
    entry.map_or(
        Snapshot {
            names: Vec::new(),
            fresh: false,
        },
        |entry| {
            let fresh = now()
                .checked_sub(entry.checked)
                .is_some_and(|age| age < TTL)
                && entry.children == children;
            Snapshot {
                names: entry.names,
                fresh,
            }
        },
    )
}
pub(super) async fn save(
    client: &Client,
    parent: &OwnedRoomId,
    children: Vec<OwnedRoomId>,
    names: Vec<(OwnedRoomId, String)>,
) {
    if names.is_empty() {
        return;
    } // A failed discovery must not erase usable names.
    let _writer = WRITER.lock().await;
    let mut cache = read(client).await;
    cache.entries.insert(
        parent.clone(),
        Entry {
            checked: now(),
            children,
            names: names
                .into_iter()
                .filter(|(_, name)| !name.trim().is_empty() && name.len() <= 1024)
                .take(2048)
                .collect(),
        },
    );
    loop {
        let Ok(bytes) = serde_json::to_vec(&cache) else {
            return;
        };
        if cache.entries.len() <= MAX_PARENTS && bytes.len() <= MAX_BYTES {
            if let Err(error) = client
                .state_store()
                .set_custom_value_no_read(&key(client), bytes)
                .await
            {
                log::debug!("Could not cache space names: {error}");
            }
            return;
        }
        let Some(oldest) = cache
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.checked)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        cache.entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::{authentication::matrix::MatrixSession, ruma::api::MatrixVersion};
    use serde_json::json;
    fn id(name: &str) -> OwnedRoomId {
        format!("!{name}:example.org").parse().unwrap()
    }
    async fn client(user: &str) -> Client {
        let client = Client::builder()
            .homeserver_url("https://example.org")
            .server_versions([MatrixVersion::V1_11])
            .build()
            .await
            .unwrap();
        client
            .restore_session(
                serde_json::from_value::<MatrixSession>(
                    json!({"user_id":user,"device_id":"CACHE","access_token":"test"}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        client
    }
    #[tokio::test]
    async fn persisted_names_refresh_on_link_changes_and_preserve_failed_lookups() {
        let client = client("@tester:example.org").await;
        let parent = id("parent");
        let child = id("child");
        save(
            &client,
            &parent,
            vec![child.clone()],
            vec![(child.clone(), "Subspace".into())],
        )
        .await;
        assert!(
            client
                .state_store()
                .get_custom_value(&key(&client))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            restore(&client).await,
            vec![(child.clone(), "Subspace".into())]
        );
        assert!(get(&client, &parent, &[child.clone()]).await.fresh);
        assert!(
            !get(&client, &parent, &[child.clone(), id("new")])
                .await
                .fresh
        );
        save(
            &client,
            &parent,
            vec![child.clone()],
            vec![(child.clone(), "Renamed".into())],
        )
        .await;
        assert_eq!(restore(&client).await[0].1, "Renamed");
        save(&client, &parent, vec![], vec![]).await;
        assert_eq!(restore(&client).await[0].1, "Renamed");
    }
    #[tokio::test]
    async fn stale_names_remain_available_and_corrupt_cache_can_be_rebuilt() {
        let client = client("@tester:example.org").await;
        let parent = id("parent");
        let child = id("child");
        save(
            &client,
            &parent,
            vec![child.clone()],
            vec![(child.clone(), "Cached".into())],
        )
        .await;
        let mut cache = read(&client).await;
        cache.entries.get_mut(&parent).unwrap().checked = now().saturating_sub(TTL + 1);
        client
            .state_store()
            .set_custom_value_no_read(&key(&client), serde_json::to_vec(&cache).unwrap())
            .await
            .unwrap();
        let stale = get(&client, &parent, &[child.clone()]).await;
        assert!(!stale.fresh);
        assert_eq!(stale.names[0].1, "Cached");
        assert_eq!(restore(&client).await[0].1, "Cached");
        client
            .state_store()
            .set_custom_value_no_read(&key(&client), b"broken".to_vec())
            .await
            .unwrap();
        assert!(restore(&client).await.is_empty());
        save(
            &client,
            &parent,
            vec![child.clone()],
            vec![(child, "Recovered".into())],
        )
        .await;
        assert_eq!(restore(&client).await[0].1, "Recovered");
    }
    #[tokio::test]
    async fn cache_is_bounded_and_keys_are_account_scoped() {
        let first = client("@tester:example.org").await;
        for n in 0..MAX_PARENTS + 2 {
            save(
                &first,
                &id(&format!("p{n}")),
                vec![],
                vec![(id(&format!("c{n}")), "Cached".into())],
            )
            .await;
        }
        assert_eq!(read(&first).await.entries.len(), MAX_PARENTS);
        assert!(
            first
                .state_store()
                .get_custom_value(&key(&first))
                .await
                .unwrap()
                .unwrap()
                .len()
                <= MAX_BYTES
        );
        let other = client("@other:example.org").await;
        assert_ne!(key(&first), key(&other));
        assert!(restore(&other).await.is_empty());
    }
}
