//! Local navigation folders, independent of Matrix space membership.
use std::collections::HashMap;

use anyhow::Result;
use gpui::{Context, Render, Window, div, prelude::*, px};
use matrix_sdk::ruma::OwnedRoomId;
use serde::{Deserialize, Serialize};
use ui::{ActiveTheme as _, Text};

#[derive(Clone)]
pub(super) struct DraggedRoom {
    pub room: OwnedRoomId,
    pub name: String,
}

impl Render for DraggedRoom {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .max_w(px(200.))
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_size(theme.text(Text::Small))
            .child(self.name.clone())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Layout {
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Group {
    pub id: u64,
    pub rooms: Vec<OwnedRoomId>,
    pub expanded: bool,
}

impl Layout {
    pub fn group_of(&self, room: &OwnedRoomId) -> Option<u64> {
        self.groups
            .iter()
            .find(|group| group.rooms.contains(room))
            .map(|group| group.id)
    }

    pub fn combine(&mut self, source: OwnedRoomId, target: OwnedRoomId) -> bool {
        if source == target {
            return false;
        }
        if let Some(group) = self.group_of(&target) {
            return self.move_into(source, group);
        }
        self.ungroup(&source);
        let id = self.groups.iter().map(|group| group.id).max().unwrap_or(0) + 1;
        self.groups.push(Group {
            id,
            rooms: vec![target, source],
            expanded: true,
        });
        true
    }

    pub fn move_into(&mut self, room: OwnedRoomId, id: u64) -> bool {
        if self.group_of(&room) == Some(id) || !self.groups.iter().any(|group| group.id == id) {
            return false;
        }
        self.ungroup(&room);
        if let Some(group) = self.groups.iter_mut().find(|group| group.id == id) {
            group.rooms.push(room);
            group.expanded = true;
        }
        true
    }

    pub fn ungroup(&mut self, room: &OwnedRoomId) -> bool {
        let present = self.group_of(room).is_some();
        for group in &mut self.groups {
            group.rooms.retain(|member| member != room);
        }
        self.groups.retain(|group| group.rooms.len() >= 2);
        present
    }

    pub fn load(user: &str) -> Result<Self> {
        Self::load_at(&path()?, user)
    }

    pub fn save(&self, user: &str) -> Result<()> {
        self.save_at(&path()?, user)
    }

    fn load_at(path: &std::path::Path, user: &str) -> Result<Self> {
        Ok(read(path)?.remove(user).unwrap_or_default())
    }

    fn save_at(&self, path: &std::path::Path, user: &str) -> Result<()> {
        let mut accounts = read(path)?;
        accounts.insert(user.to_owned(), self.clone());
        std::fs::create_dir_all(path.parent().unwrap())?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&accounts)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
}

fn path() -> Result<std::path::PathBuf> {
    Ok(dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("No data directory"))?
        .join("Huddle")
        .join("rail-groups.json"))
}

fn read(path: &std::path::Path) -> Result<HashMap<String, Layout>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn room(name: &str) -> OwnedRoomId {
        format!("!{name}:example.org").parse().unwrap()
    }

    #[test]
    fn dropping_creates_a_group_and_repeated_drops_are_noops() {
        let mut layout = Layout::default();
        assert!(!layout.combine(room("a"), room("a")));
        assert!(layout.combine(room("a"), room("b")));
        let before = layout.clone();
        assert!(!layout.combine(room("a"), room("b")));
        assert_eq!(layout, before);
        assert!(layout.combine(room("c"), room("b")));
        assert_eq!(
            layout.groups[0].rooms,
            vec![room("b"), room("a"), room("c")]
        );
    }

    #[test]
    fn moving_between_groups_dissolves_the_old_singleton() {
        let mut layout = Layout::default();
        layout.combine(room("a"), room("b"));
        layout.combine(room("c"), room("d"));
        let destination = layout.group_of(&room("d")).unwrap();
        assert!(layout.move_into(room("a"), destination));
        assert_eq!(layout.groups.len(), 1);
        assert_eq!(layout.group_of(&room("b")), None);
        assert_eq!(layout.group_of(&room("a")), Some(destination));
        assert!(layout.ungroup(&room("a")));
        assert!(layout.ungroup(&room("c")));
        assert!(layout.groups.is_empty());
    }

    #[test]
    fn persistence_preserves_members_order_and_collapse_state_per_account() {
        let path = std::env::temp_dir().join(format!(
            "huddle-rail-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut layout = Layout::default();
        assert_eq!(Layout::load_at(&path, "alice").unwrap(), Layout::default());
        layout.combine(room("space"), room("room"));
        layout.save_at(&path, "alice").unwrap();
        layout.groups[0].expanded = false;
        layout.save_at(&path, "alice").unwrap();
        Layout::default().save_at(&path, "bob").unwrap();
        assert_eq!(Layout::load_at(&path, "alice").unwrap(), layout);
        assert!(Layout::load_at(&path, "bob").unwrap().groups.is_empty());
        std::fs::write(&path, b"broken JSON").unwrap();
        assert!(layout.save_at(&path, "alice").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken JSON");
        std::fs::remove_file(path).unwrap();
    }
}
