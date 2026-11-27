//! Navigation through Matrix space links, including shared children and cycles.
use super::backend::{RoomKind, RoomSummary};
use matrix_sdk::ruma::OwnedRoomId;

pub(super) fn linked_name(
    rooms: &[RoomSummary],
    names: &std::collections::HashMap<OwnedRoomId, String>,
    id: &OwnedRoomId,
) -> String {
    rooms
        .iter()
        .find(|room| &room.id == id)
        .map(|room| &room.name)
        .filter(|name| !name.trim().is_empty() && name.as_str() != id.as_str())
        .or_else(|| names.get(id))
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

pub(super) fn children<'a>(rooms: &'a [RoomSummary], parent: &RoomSummary) -> Vec<&'a RoomSummary> {
    rooms
        .iter()
        .filter(|room| !room.invite && room.id != parent.id && parent.children.contains(&room.id))
        .collect()
}

fn joined_space<'a>(rooms: &'a [RoomSummary], id: &OwnedRoomId) -> Option<&'a RoomSummary> {
    rooms
        .iter()
        .find(|room| &room.id == id && room.kind == RoomKind::Space && !room.invite)
}

#[derive(Default)]
pub(super) struct Trail {
    ancestors: Vec<OwnedRoomId>,
}

impl Trail {
    pub fn clear(&mut self) {
        self.ancestors.clear();
    }

    pub fn reconcile(&mut self, rooms: &[RoomSummary], current: Option<&OwnedRoomId>) {
        if current.is_none() {
            self.clear();
            return;
        }
        let mut path = self.ancestors.iter().chain(current);
        if let Some(mut previous) = path.next() {
            for next in path {
                if !joined_space(rooms, previous).is_some_and(|space| space.children.contains(next))
                    || joined_space(rooms, next).is_none()
                {
                    self.clear();
                    return;
                }
                previous = next;
            }
        }
    }

    pub fn enter(
        &mut self,
        rooms: &[RoomSummary],
        current: Option<&OwnedRoomId>,
        target: &OwnedRoomId,
        nested: bool,
    ) {
        self.reconcile(rooms, current);
        if nested && current == Some(target) {
            return;
        }
        if nested
            && current
                .and_then(|id| joined_space(rooms, id))
                .is_some_and(|space| space.children.contains(target))
        {
            if let Some(index) = self.ancestors.iter().position(|id| id == target) {
                self.ancestors.truncate(index);
            } else if let Some(current) = current {
                self.ancestors.push(current.clone());
            }
        } else {
            self.clear();
        }
    }

    pub fn parent<'a>(
        &self,
        rooms: &'a [RoomSummary],
        current: &OwnedRoomId,
    ) -> Option<&'a RoomSummary> {
        self.ancestors
            .last()
            .and_then(|id| joined_space(rooms, id))
            .filter(|parent| parent.children.contains(current))
            .or_else(|| {
                rooms.iter().find(|room| {
                    room.kind == RoomKind::Space
                        && !room.invite
                        && &room.id != current
                        && room.children.contains(current)
                })
            })
    }

    pub fn back(&mut self, rooms: &[RoomSummary], current: &OwnedRoomId) -> Option<OwnedRoomId> {
        self.reconcile(rooms, Some(current));
        let parent = self.parent(rooms, current)?.id.clone();
        if self.ancestors.last() == Some(&parent) {
            self.ancestors.pop();
        } else {
            self.clear();
        }
        Some(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(name: &str) -> OwnedRoomId {
        format!("!{name}:example.org").parse().unwrap()
    }
    fn room(name: &str, kind: RoomKind, children: &[&str]) -> RoomSummary {
        RoomSummary {
            id: id(name),
            name: name.into(),
            kind,
            children: children.iter().map(|name| id(name)).collect(),
            invite: false,
            unread: 0,
            muted: false,
            can_manage: false,
            explicit_name: None,
            can_edit_name: false,
            can_edit_topic: false,
            topic: None,
            avatar: None,
            encrypted: false,
            members: 2,
        }
    }

    #[test]
    fn linked_names_use_known_names_then_discovery_then_ids() {
        let id = id("child");
        let mut known = room("child", RoomKind::Room, &[]);
        known.invite = true;
        known.name = "Invite name".into();
        let names = std::collections::HashMap::from([(id.clone(), "Discovered name".into())]);
        assert_eq!(linked_name(&[known.clone()], &names, &id), "Invite name");
        known.name = id.to_string();
        assert_eq!(linked_name(&[known], &names, &id), "Discovered name");
        assert_eq!(linked_name(&[], &Default::default(), &id), id.to_string());
    }

    #[test]
    fn nested_spaces_and_rooms_are_visible_but_self_links_and_invites_are_not() {
        let mut rooms = vec![
            room(
                "root",
                RoomKind::Space,
                &["root", "nested", "chat", "invite", "missing"],
            ),
            room("nested", RoomKind::Space, &[]),
            room("chat", RoomKind::Room, &[]),
            room("invite", RoomKind::Space, &[]),
        ];
        rooms[3].invite = true;
        let children = children(&rooms, &rooms[0]);
        assert_eq!(
            children
                .iter()
                .map(|room| room.id.clone())
                .collect::<Vec<_>>(),
            vec![id("nested"), id("chat")]
        );
    }

    #[test]
    fn shared_space_goes_back_to_the_parent_used_to_enter_it() {
        let rooms = vec![
            room("a", RoomKind::Space, &["child"]),
            room("b", RoomKind::Space, &["child"]),
            room("child", RoomKind::Space, &["leaf"]),
            room("leaf", RoomKind::Space, &[]),
        ];
        let mut trail = Trail::default();
        trail.enter(&rooms, None, &id("b"), false);
        trail.enter(&rooms, Some(&id("b")), &id("child"), true);
        trail.enter(&rooms, Some(&id("child")), &id("leaf"), true);
        assert_eq!(trail.back(&rooms, &id("leaf")), Some(id("child")));
        assert_eq!(trail.back(&rooms, &id("child")), Some(id("b")));
        assert_eq!(trail.back(&rooms, &id("b")), None);
    }

    #[test]
    fn cycles_and_removed_links_do_not_accumulate_stale_ancestors() {
        let mut rooms = vec![
            room("a", RoomKind::Space, &["b"]),
            room("b", RoomKind::Space, &["a"]),
        ];
        let mut trail = Trail::default();
        for _ in 0..10 {
            trail.enter(&rooms, Some(&id("a")), &id("b"), true);
            trail.enter(&rooms, Some(&id("b")), &id("a"), true);
        }
        assert!(trail.ancestors.is_empty());
        trail.enter(&rooms, Some(&id("a")), &id("b"), true);
        rooms[0].children.clear();
        assert_eq!(trail.back(&rooms, &id("b")), None);
        assert!(trail.ancestors.is_empty());
        trail.enter(&rooms, Some(&id("b")), &id("a"), false);
        assert!(trail.ancestors.is_empty());
    }
}
