//! Public Matrix read receipts, indexed by user/scope and last-read event.
use super::*;
use matrix_sdk::Room;
use matrix_sdk::ruma::events::receipt::{ReceiptEventContent, ReceiptThread, ReceiptType};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct Reader {
    pub user: OwnedUserId,
    pub name: String,
    pub avatar: Option<OwnedMxcUri>,
    pub event: OwnedEventId,
    pub thread: ReceiptThread,
    pub timestamp: Option<i64>,
}

pub async fn live(room: &Room, content: ReceiptEventContent) -> Vec<Reader> {
    let mut readers = Vec::new();
    for (event, receipts) in content {
        // Private receipts must never become a public "read by" indicator.
        if let Some(users) = receipts.get(&ReceiptType::Read) {
            for (user, receipt) in users {
                if user == room.own_user_id() {
                    continue;
                }
                let member = room.get_member_no_sync(user).await.ok().flatten();
                readers.push(Reader {
                    user: user.clone(),
                    name: member
                        .as_ref()
                        .map(|m| m.name().to_owned())
                        .unwrap_or_else(|| user.to_string()),
                    avatar: member
                        .as_ref()
                        .and_then(|m| m.avatar_url().map(ToOwned::to_owned)),
                    event: event.clone(),
                    thread: receipt.thread.clone(),
                    timestamp: receipt.ts.map(|ts| i64::from(ts.0)),
                });
            }
        }
    }
    readers
}

pub async fn snapshot(
    room: &Room,
    events: &[backend::Event],
    root: Option<&OwnedEventId>,
) -> Vec<Reader> {
    let scopes = match root {
        Some(root) => vec![
            ReceiptThread::Unthreaded,
            ReceiptThread::Thread(root.clone()),
        ],
        None => vec![ReceiptThread::Unthreaded, ReceiptThread::Main],
    };
    let mut result = Vec::new();
    let ids = root
        .cloned()
        .into_iter()
        .chain(events.iter().filter_map(|event| match event {
            backend::Event::Message(message) => message.id.clone(),
            _ => None,
        }));
    for id in ids {
        for scope in &scopes {
            match room
                .load_event_receipts(ReceiptType::Read, scope, &id)
                .await
            {
                Ok(receipts) => {
                    // Separate scopes can contain a receipt for the same user/event.
                    // Convert each batch before reusing the Matrix event container.
                    let content = ReceiptEventContent(
                        [(
                            id.clone(),
                            [(ReceiptType::Read, receipts.into_iter().collect())].into(),
                        )]
                        .into(),
                    );
                    result.extend(live(room, content).await);
                }
                Err(error) => log::debug!("matrix: could not load read receipts: {error}"),
            }
        }
    }
    result
}

type Key = (OwnedUserId, ReceiptThread);
#[derive(Default)]
pub struct ReadState {
    latest: HashMap<Key, Reader>,
    events: HashMap<OwnedEventId, HashSet<Key>>,
}

pub fn matches_scope(scope: &ReceiptThread, root: Option<&EventId>) -> bool {
    match scope {
        ReceiptThread::Unthreaded => true,
        ReceiptThread::Main => root.is_none(),
        ReceiptThread::Thread(thread) => root == Some(thread.as_ref()),
        _ => false,
    }
}

impl ReadState {
    pub fn merge(
        &mut self,
        readers: Vec<Reader>,
        order: impl Fn(&EventId, &EventId, &ReceiptThread) -> Option<std::cmp::Ordering>,
    ) -> Vec<OwnedEventId> {
        let mut changed = HashSet::new();
        for reader in readers {
            let key = (reader.user.clone(), reader.thread.clone());
            if let Some(old) = self.latest.get(&key) {
                let position = order(&reader.event, &old.event, &reader.thread);
                if position == Some(std::cmp::Ordering::Less)
                    || (position != Some(std::cmp::Ordering::Greater)
                        && matches!((reader.timestamp, old.timestamp), (Some(new), Some(old)) if new < old))
                {
                    continue;
                }
                changed.insert(old.event.clone());
                if let Some(keys) = self.events.get_mut(&old.event) {
                    keys.remove(&key);
                    if keys.is_empty() {
                        self.events.remove(&old.event);
                    }
                }
            }
            changed.insert(reader.event.clone());
            self.events
                .entry(reader.event.clone())
                .or_default()
                .insert(key.clone());
            self.latest.insert(key, reader);
        }
        changed.into_iter().collect()
    }

    pub fn at(
        &self,
        event: &EventId,
        root: Option<&EventId>,
        own: Option<&OwnedUserId>,
        sender: &OwnedUserId,
    ) -> Vec<&Reader> {
        self.sorted(
            self.events
                .get(event)
                .into_iter()
                .flatten()
                .filter_map(|key| self.latest.get(key)),
            root,
            own,
            sender,
        )
    }

    pub fn seen_by(
        &self,
        event: &EventId,
        root: Option<&EventId>,
        order: &[&Message],
        own: Option<&OwnedUserId>,
        sender: &OwnedUserId,
    ) -> Vec<&Reader> {
        let positions: HashMap<&EventId, usize> = order
            .iter()
            .enumerate()
            .filter_map(|(index, message)| message.id.as_deref().map(|id| (id, index)))
            .collect();
        let Some(target) = positions.get(event) else {
            return Vec::new();
        };
        self.sorted(
            self.latest.values().filter(|reader| {
                positions
                    .get::<EventId>(reader.event.as_ref())
                    .is_some_and(|position| position >= target)
            }),
            root,
            own,
            sender,
        )
    }

    fn sorted<'a>(
        &'a self,
        readers: impl Iterator<Item = &'a Reader>,
        root: Option<&EventId>,
        own: Option<&OwnedUserId>,
        sender: &OwnedUserId,
    ) -> Vec<&'a Reader> {
        let mut unique: HashMap<&OwnedUserId, &Reader> = HashMap::new();
        for reader in readers.filter(|reader| {
            Some(&reader.user) != own
                && &reader.user != sender
                && matches_scope(&reader.thread, root)
        }) {
            let previous = unique.entry(&reader.user).or_insert(reader);
            if reader.timestamp > previous.timestamp {
                *previous = reader;
            }
        }
        let mut readers: Vec<_> = unique.into_values().collect();
        readers.sort_by_cached_key(|reader| (reader.name.to_lowercase(), reader.user.clone()));
        readers
    }
}

pub fn compare(
    a: &EventId,
    b: &EventId,
    scope: &ReceiptThread,
    messages: &[Message],
    threads: &HashMap<OwnedEventId, Vec<Message>>,
) -> Option<std::cmp::Ordering> {
    let positions = |messages: &[Message]| {
        let a = messages
            .iter()
            .position(|message| message.id.as_deref() == Some(a))?;
        let b = messages
            .iter()
            .position(|message| message.id.as_deref() == Some(b))?;
        Some(a.cmp(&b))
    };
    match scope {
        ReceiptThread::Main => positions(messages),
        ReceiptThread::Unthreaded => positions(messages)
            .or_else(|| threads.values().find_map(|messages| positions(messages))),
        ReceiptThread::Thread(root) => {
            if a == root && b != root {
                Some(std::cmp::Ordering::Less)
            } else if b == root && a != root {
                Some(std::cmp::Ordering::Greater)
            } else {
                threads.get(root).and_then(|messages| positions(messages))
            }
        }
        _ => None,
    }
}

#[derive(Clone)]
pub struct Selection {
    pub room: OwnedRoomId,
    pub event: OwnedEventId,
    pub root: Option<OwnedEventId>,
    pub limit: usize,
}

impl MatrixClient {
    pub(super) fn receipt_line(
        &self,
        message: &Message,
        spot: Spot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let room = self.room.as_ref()?;
        let event = message.id.as_deref()?;
        let root = if spot == Spot::Thread {
            self.thread.as_deref()
        } else {
            None
        };
        let timeline = self.timelines.get(room)?;
        let own = self.me.as_ref().map(|me| &me.id);
        let readers =
            if self.own_read_targets[usize::from(spot == Spot::Thread)].as_deref() == Some(event) {
                let mut order = Vec::new();
                if let Some(root) = root {
                    order.extend(
                        timeline
                            .messages
                            .iter()
                            .filter(|message| message.id.as_deref() == Some(root)),
                    );
                    if let Some(replies) = timeline.threads.get(root) {
                        order.extend(replies.iter());
                    }
                } else {
                    order.extend(timeline.messages.iter());
                }
                timeline
                    .receipts
                    .seen_by(event, root, &order, own, &message.sender)
            } else {
                timeline.receipts.at(event, root, own, &message.sender)
            };
        if readers.is_empty() {
            return None;
        }
        let names = readers
            .iter()
            .take(3)
            .map(|reader| reader.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let label = if readers.len() > 3 {
            format!("Read by {names} and {} others", readers.len() - 3)
        } else {
            format!("Read by {names}")
        };
        let selection = Selection {
            room: room.clone(),
            event: event.to_owned(),
            root: root.map(ToOwned::to_owned),
            limit: 50,
        };
        Some(
            div()
                .id(SharedString::from(format!(
                    "receipt-{}-{event}",
                    if spot == Spot::Main { "main" } else { "thread" }
                )))
                .flex()
                .items_center()
                .gap_1()
                .mt_1()
                .text_size(px(11.))
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(cx.theme().primary))
                .child(icon("icons/circle-check.svg", px(12.), cx.theme().primary))
                .child(div().min_w_0().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.readers_view = Some(selection.clone());
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_readers(
        &self,
        selected: &Selection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let timeline = self.timelines.get(&selected.room);
        let message = timeline.and_then(|timeline| timeline.find(&selected.event));
        let mut order = Vec::new();
        if let Some(timeline) = timeline {
            if let Some(root) = &selected.root {
                order.extend(
                    timeline
                        .messages
                        .iter()
                        .filter(|message| message.id.as_ref() == Some(root)),
                );
                if let Some(replies) = timeline.threads.get(root) {
                    order.extend(replies.iter());
                }
            } else {
                order.extend(timeline.messages.iter());
            }
        }
        let readers = match (timeline, message) {
            (Some(timeline), Some(message)) => timeline.receipts.seen_by(
                &selected.event,
                selected.root.as_deref(),
                &order,
                self.me.as_ref().map(|me| &me.id),
                &message.sender,
            ),
            _ => Vec::new(),
        };
        let mut modal = Modal::new("message-readers", "Read by")
            .w(px(480.))
            .close_button()
            .detail("Only shared read receipts are shown.")
            .on_dismiss(cx.listener(|this, _: &(), _, cx| {
                this.readers_view = None;
                cx.notify();
            }));
        if readers.is_empty() {
            modal = modal.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No public read receipts yet."),
            );
        }
        let count = readers.len();
        for reader in readers.into_iter().take(selected.limit) {
            let person = self.person_of(&reader.user, &reader.name, reader.avatar.as_ref());
            modal = modal.child(
                div()
                    .id(SharedString::from(format!("read-by-{}", reader.user)))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .child(self.face(
                        reader.user.as_str(),
                        &reader.name,
                        reader.avatar.as_ref(),
                        px(28.),
                    ))
                    .child(
                        div().flex_1().min_w_0().child(reader.name.clone()).child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(reader.user.to_string()),
                        ),
                    )
                    .when_some(reader.timestamp, |row, timestamp| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(stamp(timestamp)),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.readers_view = None;
                        this.open_person(person.clone(), cx);
                    })),
            );
        }
        if count > selected.limit {
            modal = modal.child(
                Button::new("readers-more")
                    .ghost()
                    .label(format!("Show more ({count} people)"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(view) = &mut this.readers_view {
                            view.limit += 50;
                        }
                        cx.notify();
                    })),
            );
        }
        modal
            .action(
                Button::new("readers-close")
                    .label("Close")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.readers_view = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reader(user: &str, event: &str, thread: ReceiptThread, timestamp: i64) -> Reader {
        Reader {
            user: user.try_into().unwrap(),
            name: user.into(),
            avatar: None,
            event: event.try_into().unwrap(),
            thread,
            timestamp: Some(timestamp),
        }
    }
    fn message(id: &str) -> Message {
        Message {
            id: Some(id.try_into().unwrap()),
            local: None,
            sender: "@sender:example.org".try_into().unwrap(),
            sender_name: "Sender".into(),
            avatar: None,
            body: "Hi".into(),
            rich: rich::plain("Hi"),
            kind: MessageKind::Text,
            picture: None,
            timestamp: 0,
            edited: false,
            reply: None,
            thread: None,
            replies: 0,
            reactions: Vec::new(),
            mentioned: false,
        }
    }
    fn merge(state: &mut ReadState, readers: Vec<Reader>, messages: &[Message]) {
        state.merge(readers, |a, b, scope| {
            compare(a, b, scope, messages, &HashMap::new())
        });
    }

    #[test]
    fn moving_a_read_marker_removes_the_previous_indicator() {
        let messages = [message("$one"), message("$two")];
        let sender = &messages[0].sender;
        let mut state = ReadState::default();
        merge(
            &mut state,
            vec![reader("@alice:example.org", "$one", ReceiptThread::Main, 1)],
            &messages,
        );
        assert_eq!(
            state
                .at(messages[0].id.as_ref().unwrap(), None, None, sender)
                .len(),
            1
        );
        merge(
            &mut state,
            vec![reader("@alice:example.org", "$two", ReceiptThread::Main, 2)],
            &messages,
        );
        assert!(
            state
                .at(messages[0].id.as_ref().unwrap(), None, None, sender)
                .is_empty()
        );
        assert_eq!(
            state
                .at(messages[1].id.as_ref().unwrap(), None, None, sender)
                .len(),
            1
        );
        assert_eq!(state.events.len(), 1);
    }

    #[test]
    fn later_receipts_include_earlier_messages_but_unknown_events_do_not() {
        let messages = [message("$one"), message("$two"), message("$three")];
        let mut state = ReadState::default();
        merge(
            &mut state,
            vec![
                reader("@alice:example.org", "$two", ReceiptThread::Main, 5),
                reader(
                    "@unknown:example.org",
                    "$not-loaded",
                    ReceiptThread::Main,
                    9999,
                ),
            ],
            &messages,
        );
        let order: Vec<_> = messages.iter().collect();
        for message in &messages[..2] {
            let readers = state.seen_by(
                message.id.as_ref().unwrap(),
                None,
                &order,
                None,
                &message.sender,
            );
            assert_eq!(readers.len(), 1);
            assert_eq!(readers[0].user.as_str(), "@alice:example.org");
        }
        assert!(
            state
                .seen_by(
                    messages[2].id.as_ref().unwrap(),
                    None,
                    &order,
                    None,
                    &messages[2].sender
                )
                .is_empty()
        );
    }

    #[test]
    fn stale_snapshots_cannot_move_a_marker_backwards_even_with_a_later_clock() {
        let messages = [message("$one"), message("$two")];
        let mut state = ReadState::default();
        merge(
            &mut state,
            vec![reader(
                "@alice:example.org",
                "$two",
                ReceiptThread::Main,
                50,
            )],
            &messages,
        );
        merge(
            &mut state,
            vec![reader(
                "@alice:example.org",
                "$one",
                ReceiptThread::Main,
                100,
            )],
            &messages,
        );
        assert!(state.events.contains_key(messages[1].id.as_ref().unwrap()));
        let mut state = ReadState::default();
        merge(
            &mut state,
            vec![reader(
                "@alice:example.org",
                "$one",
                ReceiptThread::Main,
                100,
            )],
            &messages,
        );
        merge(
            &mut state,
            vec![reader(
                "@alice:example.org",
                "$two",
                ReceiptThread::Main,
                50,
            )],
            &messages,
        );
        assert!(state.events.contains_key(messages[1].id.as_ref().unwrap()));
    }

    #[test]
    fn thread_receipts_are_isolated_from_the_main_timeline_and_other_threads() {
        let root: OwnedEventId = "$root".try_into().unwrap();
        let other: OwnedEventId = "$other".try_into().unwrap();
        assert!(!matches_scope(&ReceiptThread::Thread(root.clone()), None));
        assert!(!matches_scope(
            &ReceiptThread::Thread(root.clone()),
            Some(&other)
        ));
        assert!(matches_scope(
            &ReceiptThread::Thread(root.clone()),
            Some(&root)
        ));
        assert!(!matches_scope(&ReceiptThread::Main, Some(&root)));
        assert!(matches_scope(&ReceiptThread::Unthreaded, Some(&root)));
    }

    #[test]
    fn readers_are_deduplicated_across_scopes_and_exclude_self_and_sender() {
        let messages = [message("$one")];
        let mut state = ReadState::default();
        let mut older = reader("@alice:example.org", "$one", ReceiptThread::Main, 1);
        older.name = "Old name".into();
        let mut newer = reader("@alice:example.org", "$one", ReceiptThread::Unthreaded, 2);
        newer.name = "Alice".into();
        merge(
            &mut state,
            vec![
                older,
                newer,
                reader("@self:example.org", "$one", ReceiptThread::Main, 2),
                reader("@sender:example.org", "$one", ReceiptThread::Main, 2),
            ],
            &messages,
        );
        let own = "@self:example.org".try_into().unwrap();
        let readers = state.at(
            messages[0].id.as_ref().unwrap(),
            None,
            Some(&own),
            &messages[0].sender,
        );
        assert_eq!(readers.len(), 1);
        assert_eq!(readers[0].name, "Alice");
    }
}
