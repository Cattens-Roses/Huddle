//! Room and space administration through the Matrix client API.
use super::*;
use matrix_sdk::ruma::events::{
    StateEventType,
    room::power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent, UserPowerLevel},
};
use matrix_sdk::ruma::{Int, OwnedRoomAliasId};
use serde_json::{Value, json};

#[derive(Clone, Default)]
pub struct Permissions {
    pub name: bool,
    pub topic: bool,
    pub avatar: bool,
    pub join: bool,
    pub history: bool,
    pub guest: bool,
    pub encryption: bool,
    pub alias: bool,
    pub power: bool,
    pub children: bool,
    pub invite: bool,
    pub kick: bool,
    pub ban: bool,
}
impl Permissions {
    pub fn new(levels: &RoomPowerLevels, user: &UserId) -> Self {
        let state = |kind| levels.user_can_send_state(user, kind);
        Self {
            name: state(StateEventType::RoomName),
            topic: state(StateEventType::RoomTopic),
            avatar: state(StateEventType::RoomAvatar),
            join: state(StateEventType::RoomJoinRules),
            history: state(StateEventType::RoomHistoryVisibility),
            guest: state(StateEventType::RoomGuestAccess),
            encryption: state(StateEventType::RoomEncryption),
            alias: state(StateEventType::RoomCanonicalAlias),
            power: state(StateEventType::RoomPowerLevels),
            children: state(StateEventType::SpaceChild),
            invite: levels.user_can_invite(user),
            kick: levels.user_can_kick(user),
            ban: levels.user_can_ban(user),
        }
    }
    pub fn any(&self) -> bool {
        self.name
            || self.topic
            || self.avatar
            || self.join
            || self.history
            || self.guest
            || self.encryption
            || self.alias
            || self.power
            || self.children
            || self.kick
            || self.ban
    }
}

pub const POWER_LABELS: [&str; 8] = [
    "Default member level",
    "Send messages",
    "Edit room settings",
    "Invite people",
    "Remove members",
    "Ban members",
    "Delete others' messages",
    "Notify the whole room",
];

pub struct AdminMember {
    pub id: OwnedUserId,
    pub name: String,
    pub membership: String,
    pub power: String,
}
pub struct Child {
    pub id: OwnedRoomId,
    pub order: String,
    pub suggested: bool,
}
pub struct Snapshot {
    pub permissions: Permissions,
    pub space: bool,
    pub version: String,
    pub avatar: Option<OwnedMxcUri>,
    pub join: String,
    pub allowed_spaces: String,
    pub history: String,
    pub guest: String,
    pub encrypted: bool,
    pub directory: Option<String>,
    pub alias: String,
    pub aliases: Vec<String>,
    pub powers: [i64; 8],
    pub members: Vec<AdminMember>,
    pub children: Vec<Child>,
}

#[derive(Clone, Copy)]
pub enum Setting {
    History,
    Guest,
    Directory,
}
#[derive(Clone)]
pub enum MemberAction {
    Invite,
    Kick,
    Ban,
    Unban,
    Power(i64),
}
#[derive(Clone)]
pub enum Action {
    CreateRoom(String),
    Setting(Setting, String),
    Join {
        rule: String,
        spaces: Vec<OwnedRoomId>,
    },
    Avatar {
        bytes: Arc<[u8]>,
        mime: String,
    },
    RemoveAvatar,
    EnableEncryption,
    Alias {
        alias: String,
        remove: bool,
    },
    Powers([i64; 8]),
    Member {
        user: OwnedUserId,
        action: MemberAction,
        reason: String,
    },
    Child {
        id: OwnedRoomId,
        order: String,
        suggested: bool,
        remove: bool,
    },
}

fn defaults(levels: &RoomPowerLevels) -> [i64; 8] {
    [
        levels.users_default,
        levels.events_default,
        levels.state_default,
        levels.invite,
        levels.kick,
        levels.ban,
        levels.redact,
        levels.notifications.room,
    ]
    .map(i64::from)
}

pub async fn load(client: &Client, id: &OwnedRoomId) -> Result<Snapshot> {
    let room = joined(client, id)?;
    let user = client.user_id().ok_or_else(|| anyhow!("not signed in"))?;
    let state = current_state(client, id).await?;
    let levels = current_levels(&room, &state)?;
    let permissions = Permissions::new(&levels, user);
    anyhow::ensure!(
        permissions.any(),
        "You don't have permission to manage this room."
    );
    let content = |kind: &str| {
        state
            .iter()
            .find(|event| event["type"] == kind && event["state_key"] == "")
            .map(|event| event["content"].clone())
            .unwrap_or_else(|| json!({}))
    };
    let join = content("m.room.join_rules");
    let allowed_spaces = join
        .get("allow")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|allow| allow.get("room_id").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let mut members = state
        .iter()
        .filter(|event| event["type"] == "m.room.member")
        .filter_map(|event| {
            let membership = event["content"]["membership"].as_str()?;
            if !["join", "invite", "knock", "ban"].contains(&membership) {
                return None;
            }
            let id: OwnedUserId = event["state_key"].as_str()?.try_into().ok()?;
            Some(AdminMember {
                name: event["content"]["displayname"]
                    .as_str()
                    .unwrap_or(id.as_str())
                    .to_owned(),
                membership: membership.to_owned(),
                power: match levels.for_user(&id) {
                    UserPowerLevel::Int(value) => value.to_string(),
                    _ => "Creator".into(),
                },
                id,
            })
        })
        .collect::<Vec<_>>();
    members.sort_by_key(|member| member.name.to_lowercase());
    let mut children = Vec::new();
    if room.is_space() {
        for event in &state {
            if event["type"] == "m.space.child"
                && event["content"]["via"]
                    .as_array()
                    .is_some_and(|via| !via.is_empty())
                && let Some(id) = event["state_key"]
                    .as_str()
                    .and_then(|id| OwnedRoomId::try_from(id).ok())
            {
                children.push(Child {
                    id,
                    order: event["content"]["order"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    suggested: event["content"]["suggested"].as_bool().unwrap_or(false),
                });
            }
        }
        children.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    }
    use matrix_sdk::ruma::api::client::directory::get_room_visibility;
    let directory = client
        .send(get_room_visibility::v3::Request::new(id.clone()))
        .await
        .ok()
        .map(|r| r.visibility.to_string());
    Ok(Snapshot {
        permissions,
        space: room.is_space(),
        version: room.version().map(|v| v.to_string()).unwrap_or_default(),
        avatar: content("m.room.avatar")["url"]
            .as_str()
            .and_then(|url| OwnedMxcUri::try_from(url).ok()),
        join: join
            .get("join_rule")
            .and_then(Value::as_str)
            .unwrap_or("invite")
            .to_owned(),
        allowed_spaces,
        history: content("m.room.history_visibility")["history_visibility"]
            .as_str()
            .unwrap_or("shared")
            .to_owned(),
        guest: content("m.room.guest_access")["guest_access"]
            .as_str()
            .unwrap_or("forbidden")
            .to_owned(),
        encrypted: content("m.room.encryption")["algorithm"].is_string(),
        directory,
        alias: content("m.room.canonical_alias")["alias"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        aliases: content("m.room.canonical_alias")["alt_aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|alias| alias.as_str().map(str::to_owned))
            .collect(),
        powers: defaults(&levels),
        members,
        children,
    })
}

async fn current_state(client: &Client, id: &matrix_sdk::ruma::RoomId) -> Result<Vec<Value>> {
    use matrix_sdk::ruma::api::client::state::get_state_events;
    let state = client
        .send(get_state_events::v3::Request::new(id.to_owned()))
        .await?;
    state
        .room_state
        .iter()
        .map(|event| serde_json::from_str(event.json().get()).map_err(Into::into))
        .collect()
}

fn current_levels(room: &Room, state: &[Value]) -> Result<RoomPowerLevels> {
    let content = state
        .iter()
        .find(|event| event["type"] == "m.room.power_levels" && event["state_key"] == "")
        .ok_or_else(|| anyhow!("Room permissions are unavailable. Refresh and try again."))?;
    let content: RoomPowerLevelsEventContent = serde_json::from_value(content["content"].clone())?;
    let rules = room
        .version()
        .and_then(|version| version.rules())
        .ok_or_else(|| anyhow!("Unsupported room version."))?;
    Ok(RoomPowerLevels::new(
        content.into(),
        &rules.authorization,
        room.creators().unwrap_or_default(),
    ))
}

pub(super) async fn fresh_levels(client: &Client, room: &Room) -> Result<RoomPowerLevels> {
    current_levels(room, &current_state(client, room.room_id()).await?)
}

fn require(allowed: bool) -> Result<()> {
    anyhow::ensure!(allowed, "You don't have permission to change this setting.");
    Ok(())
}

pub async fn apply(client: &Client, id: &OwnedRoomId, action: Action) -> Result<()> {
    let room = joined(client, id)?;
    let user = client.user_id().ok_or_else(|| anyhow!("not signed in"))?;
    let state = current_state(client, id).await?;
    let levels = current_levels(&room, &state)?;
    let p = Permissions::new(&levels, user);
    match action {
        Action::CreateRoom(name) => {
            require(p.children)?;
            anyhow::ensure!(
                room.is_space(),
                "Choose a space before creating a linked room."
            );
            anyhow::ensure!(!name.trim().is_empty(), "Enter a name for the new room.");
            let mut request = create_room::v3::Request::new();
            request.name = Some(name.trim().to_owned());
            let child = client.create_room(request).await?;
            child.send_state_event_raw("m.space.parent", id.as_str(), json!({"canonical":true,"via":[user.server_name()]})).await
                .context("Room was created, but its parent space could not be set. The room is still available in your room list.")?;
            room.send_state_event_raw("m.space.child", child.room_id().as_str(), json!({"via":[user.server_name()],"suggested":false})).await
                .context("Room was created, but it could not be linked to this space. Add it from your room list.")?;
        }
        Action::Setting(setting, value) => match setting {
            Setting::History => {
                require(p.history)?;
                anyhow::ensure!(
                    ["joined", "invited", "shared", "world_readable"].contains(&value.as_str()),
                    "Invalid history visibility."
                );
                room.send_state_event_raw(
                    "m.room.history_visibility",
                    "",
                    json!({"history_visibility":value}),
                )
                .await?;
            }
            Setting::Guest => {
                require(p.guest)?;
                anyhow::ensure!(
                    ["can_join", "forbidden"].contains(&value.as_str()),
                    "Invalid guest access."
                );
                room.send_state_event_raw("m.room.guest_access", "", json!({"guest_access":value}))
                    .await?;
            }
            Setting::Directory => {
                require(p.join)?;
                use matrix_sdk::ruma::api::client::directory::set_room_visibility;
                use matrix_sdk::ruma::api::client::room::Visibility;
                let visibility = match value.as_str() {
                    "public" => Visibility::Public,
                    "private" => Visibility::Private,
                    _ => anyhow::bail!("Invalid directory visibility."),
                };
                client
                    .send(set_room_visibility::v3::Request::new(
                        id.clone(),
                        visibility,
                    ))
                    .await?;
            }
        },
        Action::Join { rule, spaces } => {
            require(p.join)?;
            anyhow::ensure!(
                [
                    "invite",
                    "public",
                    "knock",
                    "restricted",
                    "knock_restricted"
                ]
                .contains(&rule.as_str()),
                "Invalid join rule."
            );
            let mut content = json!({"join_rule":rule});
            if rule == "restricted" || rule == "knock_restricted" {
                anyhow::ensure!(!spaces.is_empty(), "Choose at least one allowed space.");
                anyhow::ensure!(
                    !spaces.contains(id),
                    "A room cannot grant access through itself."
                );
                content["allow"] = json!(
                    spaces
                        .iter()
                        .map(|id| json!({"type":"m.room_membership", "room_id":id}))
                        .collect::<Vec<_>>()
                );
            }
            room.send_state_event_raw("m.room.join_rules", "", content)
                .await?;
        }
        Action::Avatar { bytes, mime } => {
            require(p.avatar)?;
            anyhow::ensure!(
                bytes.len() <= super::super::UPLOAD_LIMIT,
                "That file is too large. Choose one under 20 MB."
            );
            let mime: mime::Mime = mime.parse()?;
            anyhow::ensure!(
                mime.type_() == mime::IMAGE,
                "Choose an image for the room icon."
            );
            let uploaded = client.media().upload(&mime, bytes.to_vec(), None).await?;
            room.set_avatar_url(&uploaded.content_uri, None).await?;
        }
        Action::RemoveAvatar => {
            require(p.avatar)?;
            room.send_state_event_raw("m.room.avatar", "", json!({}))
                .await?;
        }
        Action::EnableEncryption => {
            require(p.encryption)?;
            anyhow::ensure!(!room.is_space(), "Spaces cannot be encrypted.");
            room.enable_encryption().await?;
        }
        Action::Alias { alias, remove } => {
            require(p.alias)?;
            let alias: OwnedRoomAliasId = alias
                .trim()
                .try_into()
                .context("Enter a full address such as #room:matrix.org.")?;
            let content = state
                .iter()
                .find(|event| event["type"] == "m.room.canonical_alias")
                .map(|event| &event["content"]);
            let mut aliases: Vec<OwnedRoomAliasId> = content
                .and_then(|c| c["alt_aliases"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().and_then(|a| a.try_into().ok()))
                .collect();
            let mut canonical: Option<OwnedRoomAliasId> = content
                .and_then(|c| c["alias"].as_str())
                .and_then(|a| a.try_into().ok());
            if remove {
                let resolved = client.resolve_room_alias(&alias).await?;
                anyhow::ensure!(
                    resolved.room_id == *id,
                    "That address belongs to another room."
                );
                // Stop advertising the address before deleting its directory mapping.
                aliases.retain(|other| other != &alias);
                if canonical.as_ref() == Some(&alias) {
                    canonical = None;
                }
                let mut content = json!({"alt_aliases":aliases});
                if let Some(alias) = canonical {
                    content["alias"] = json!(alias);
                }
                room.send_state_event_raw("m.room.canonical_alias", "", content)
                    .await?;
                client.remove_room_alias(&alias).await?;
            } else {
                match client.resolve_room_alias(&alias).await {
                    Ok(resolved) => anyhow::ensure!(
                        resolved.room_id == *id,
                        "That address belongs to another room."
                    ),
                    Err(error) if error.client_api_error_kind() == Some(&ErrorKind::NotFound) => {
                        client.create_room_alias(&alias, id).await?;
                    }
                    Err(error) => return Err(error.into()),
                }
                if let Some(previous) = canonical.filter(|old| old != &alias)
                    && !aliases.contains(&previous)
                {
                    aliases.push(previous);
                }
                aliases.retain(|other| other != &alias);
                room.send_state_event_raw(
                    "m.room.canonical_alias",
                    "",
                    json!({"alias":alias,"alt_aliases":aliases}),
                )
                .await?;
            }
        }
        Action::Powers(values) => {
            require(p.power)?;
            let old = defaults(&levels);
            for (value, old) in values.iter().zip(old) {
                let new = Int::try_from(*value)?;
                if *value != old {
                    require(
                        levels.for_user(user) >= Int::try_from(old)?
                            && levels.for_user(user) >= new,
                    )?;
                }
            }
            let values = values
                .map(Int::try_from)
                .into_iter()
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut content = RoomPowerLevelsEventContent::try_from(levels)?;
            content.users_default = values[0];
            content.events_default = values[1];
            content.state_default = values[2];
            content.invite = values[3];
            content.kick = values[4];
            content.ban = values[5];
            content.redact = values[6];
            content.notifications.room = values[7];
            room.send_state_event(content).await?;
        }
        Action::Member {
            user: target,
            action,
            reason,
        } => {
            let reason = (!reason.trim().is_empty()).then_some(reason.as_str());
            match action {
                MemberAction::Invite => {
                    require(p.invite)?;
                    room.invite_user_by_id(&target).await?;
                }
                MemberAction::Kick => {
                    require(target != user && levels.user_can_kick_user(user, &target))?;
                    room.kick_user(&target, reason).await?;
                }
                MemberAction::Ban => {
                    require(target != user && levels.user_can_ban_user(user, &target))?;
                    room.ban_user(&target, reason).await?;
                }
                MemberAction::Unban => {
                    require(levels.user_can_unban_user(user, &target))?;
                    room.unban_user(&target, reason).await?;
                }
                MemberAction::Power(value) => {
                    let value = Int::try_from(value)?;
                    require(
                        levels.user_can_change_user_power_level(user, &target)
                            && levels.for_user(user) >= value,
                    )?;
                    let mut content = RoomPowerLevelsEventContent::try_from(levels)?;
                    content.users.insert(target, value);
                    room.send_state_event(content).await?;
                }
            }
        }
        Action::Child {
            id: child,
            order,
            suggested,
            remove,
        } => {
            require(p.children)?;
            anyhow::ensure!(
                room.is_space() && child != *id,
                "Choose another room to add to this space."
            );
            anyhow::ensure!(
                order.len() <= 50 && order.bytes().all(|byte| (0x20..=0x7e).contains(&byte)),
                "Room order must use at most 50 printable ASCII characters."
            );
            let content = if remove {
                json!({"via":[]})
            } else {
                let mut via = if let Some(linked) = client.get_room(&child) {
                    linked.route().await.unwrap_or_default()
                } else {
                    Vec::new()
                };
                if via.is_empty() {
                    if let Some(server) = child.server_name() {
                        via.push(server.to_owned());
                    } else if let Some(existing) = state.iter().find(|event| {
                        event["type"] == "m.space.child" && event["state_key"] == child.as_str()
                    }) {
                        via = existing["content"]["via"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|server| {
                                server.as_str().and_then(|server| {
                                    matrix_sdk::ruma::OwnedServerName::try_from(server).ok()
                                })
                            })
                            .collect();
                    }
                }
                anyhow::ensure!(
                    !via.is_empty(),
                    "Join this room before adding it to the space so its participating servers can be found."
                );
                json!({"via":via,"order":order,"suggested":suggested})
            };
            room.send_state_event_raw("m.space.child", child.as_str(), content)
                .await?;
        }
    }
    Ok(())
}
