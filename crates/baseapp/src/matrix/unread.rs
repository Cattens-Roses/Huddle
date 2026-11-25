use super::*;
use std::collections::HashSet;

/// Count each joined room once, even in spaces with shared children or cycles.
pub(super) fn home(rooms: &[RoomSummary]) -> u64 {
    rooms
        .iter()
        .filter(|room| room.kind == RoomKind::Direct && !room.invite)
        .fold(0u64, |total, room| total.saturating_add(room.unread))
}

pub(super) fn space(rooms: &[RoomSummary], root: &OwnedRoomId) -> u64 {
    group(rooms, std::iter::once(root))
}

pub(super) fn group<'a>(
    rooms: &[RoomSummary],
    roots: impl IntoIterator<Item = &'a OwnedRoomId>,
) -> u64 {
    let known: HashMap<_, _> = rooms.iter().map(|room| (&room.id, room)).collect();
    let mut pending: Vec<_> = roots.into_iter().collect();
    let mut visited = HashSet::new();
    let mut total = 0u64;
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(room) = known.get(id).filter(|room| !room.invite) else {
            continue;
        };
        total = total.saturating_add(room.unread);
        if room.kind == RoomKind::Space {
            pending.extend(room.children.iter());
        }
    }
    total
}

pub(super) fn badge(count: u64, cx: &App) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(18.))
        .h(px(18.))
        .px(px(5.))
        .rounded_full()
        .bg(cx.theme().danger)
        .text_color(cx.theme().danger_foreground)
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .child(count.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn room(name: &str, kind: RoomKind, unread: u64, children: &[&str]) -> RoomSummary {
        RoomSummary {
            id: format!("!{name}:example.org").parse().unwrap(),
            name: name.into(),
            kind,
            unread,
            children: children
                .iter()
                .map(|name| format!("!{name}:example.org").parse().unwrap())
                .collect(),
            can_manage: false,
            explicit_name: None,
            can_edit_name: false,
            can_edit_topic: false,
            topic: None,
            avatar: None,
            invite: false,
            encrypted: false,
            members: 2,
        }
    }
    #[test]
    fn home_counts_messages_in_dms_and_excludes_invites_and_channels() {
        let mut rooms = vec![
            room("dm1", RoomKind::Direct, 3, &[]),
            room("dm2", RoomKind::Direct, 5, &[]),
            room("channel", RoomKind::Room, 20, &[]),
            room("invite", RoomKind::Direct, 8, &[]),
        ];
        rooms[3].invite = true;
        assert_eq!(home(&rooms), 8);
        rooms[0].unread = 0;
        assert_eq!(home(&rooms), 5);
        rooms[1].unread = 0;
        assert_eq!(home(&rooms), 0);
    }
    #[test]
    fn folders_deduplicate_rooms_shared_by_spaces() {
        let rooms = vec![
            room("space1", RoomKind::Space, 0, &["shared", "other"]),
            room("space2", RoomKind::Space, 0, &["shared"]),
            room("shared", RoomKind::Room, 4, &[]),
            room("other", RoomKind::Room, 7, &[]),
        ];
        assert_eq!(
            group(&rooms, [&rooms[0].id, &rooms[1].id, &rooms[2].id]),
            11
        );
    }

    #[test]
    fn space_counts_nested_rooms_once_and_handles_cycles_and_unknown_children() {
        let mut rooms = vec![
            room(
                "root",
                RoomKind::Space,
                0,
                &["nested", "channel", "missing"],
            ),
            room("nested", RoomKind::Space, 0, &["root", "channel", "other"]),
            room("channel", RoomKind::Room, 4, &[]),
            room("other", RoomKind::Room, 7, &[]),
            room("unrelated", RoomKind::Room, 99, &[]),
        ];
        assert_eq!(space(&rooms, &rooms[0].id), 11);
        rooms[3].invite = true;
        assert_eq!(space(&rooms, &rooms[0].id), 4);
        rooms[2].unread = 0;
        assert_eq!(space(&rooms, &rooms[0].id), 0);
    }
}
