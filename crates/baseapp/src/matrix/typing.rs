use matrix_sdk::ruma::OwnedRoomId;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub(super) const IDLE: Duration = Duration::from_secs(4);
const REFRESH: Duration = Duration::from_millis(3100);
const EXPIRES: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct Local {
    active: Option<(OwnedRoomId, Instant)>,
}

impl Local {
    pub fn activity(&mut self, room: &OwnedRoomId, now: Instant) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|(id, sent)| id == room && now.duration_since(*sent) < REFRESH)
        {
            return false;
        }
        self.active = Some((room.clone(), now));
        true
    }

    pub fn stop(&mut self) -> Option<OwnedRoomId> {
        self.active.take().map(|(room, _)| room)
    }
}

#[derive(Default)]
pub(super) struct Remote(HashMap<OwnedRoomId, (String, Instant)>);

impl Remote {
    pub fn update(&mut self, room: OwnedRoomId, names: Vec<String>, now: Instant) {
        match label(&names) {
            Some(label) => {
                self.0.insert(room, (label, now + EXPIRES));
            }
            None => {
                self.0.remove(&room);
            }
        }
    }
    pub fn label(&self, room: &OwnedRoomId, now: Instant) -> Option<&str> {
        self.0
            .get(room)
            .filter(|(_, until)| *until > now)
            .map(|(label, _)| label.as_str())
    }
    pub fn expire(&mut self, now: Instant) {
        self.0.retain(|_, (_, until)| *until > now);
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn label(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [name] => Some(format!("{name} is typing…")),
        [first, second] => Some(format!("{first} and {second} are typing…")),
        _ => Some("Several people are typing…".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::room_id;

    #[test]
    fn local_notices_are_throttled_and_restart_after_stopping() {
        let room = room_id!("!chat:example.org").to_owned();
        let now = Instant::now();
        let mut local = Local::default();
        assert!(local.activity(&room, now));
        assert!(!local.activity(&room, now + Duration::from_secs(1)));
        assert!(local.activity(&room, now + REFRESH));
        assert_eq!(local.stop(), Some(room.clone()));
        assert_eq!(local.stop(), None);
        assert!(local.activity(&room, now + REFRESH));
    }

    #[test]
    fn remote_snapshots_replace_names_clear_and_expire_per_room() {
        let room = room_id!("!chat:example.org").to_owned();
        let other = room_id!("!other:example.org").to_owned();
        let now = Instant::now();
        let mut remote = Remote::default();
        remote.update(room.clone(), vec!["Alice".into()], now);
        remote.update(
            other.clone(),
            vec!["Bob".into(), "Carol".into()],
            now + IDLE,
        );
        assert_eq!(remote.label(&room, now), Some("Alice is typing…"));
        remote.update(room.clone(), vec!["Dave".into()], now);
        assert_eq!(remote.label(&room, now), Some("Dave is typing…"));
        remote.expire(now + EXPIRES);
        assert_eq!(remote.label(&room, now + EXPIRES), None);
        assert_eq!(
            remote.label(&other, now + EXPIRES),
            Some("Bob and Carol are typing…")
        );
        remote.update(other, vec![], now + EXPIRES);
        assert!(remote.is_empty());
        assert_eq!(
            label(&["A".into(), "B".into(), "C".into()]),
            Some("Several people are typing…".into())
        );
    }
}
