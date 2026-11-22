//! Profile details supported by the homeserver, plus room-specific nicknames.
use super::*;
use matrix_sdk::ruma::profile::{ProfileFieldName, ProfileFieldValue};
use matrix_sdk::{Client, RoomState};
use serde_json::json;

pub const KEYS: [&str; 4] = [
    "app.huddle.profile.bio",
    "app.huddle.profile.pronouns",
    "app.huddle.profile.website",
    "m.tz",
];
pub const LABELS: [&str; 4] = ["About me", "Pronouns", "Website", "Time zone"];
pub const LIMITS: [usize; 4] = [1000, 80, 2048, 100];

#[derive(Clone, Default)]
pub struct Public {
    pub values: [String; 4],
}
impl Public {
    pub fn read(
        profile: &matrix_sdk::ruma::api::client::profile::get_profile::v3::Response,
    ) -> Self {
        Self {
            values: KEYS.map(|key| {
                profile
                    .get(key)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned()
            }),
        }
    }
    pub fn bytes(&self) -> usize {
        self.values.iter().map(String::len).sum()
    }
}
pub struct RoomProfile {
    pub id: OwnedRoomId,
    pub name: String,
    pub nickname: String,
}
pub struct Details {
    pub public: Public,
    pub allowed: [bool; 4],
    pub can_name: bool,
    pub can_avatar: bool,
    pub name: String,
    pub avatar: Option<OwnedMxcUri>,
    pub room: Option<RoomProfile>,
}
pub enum AvatarEdit {
    Keep,
    Remove,
    Upload { bytes: Arc<[u8]>, mime: String },
}
pub struct Edit {
    pub name: Option<String>,
    pub avatar: AvatarEdit,
    pub fields: [Option<String>; 4],
    pub room: Option<(OwnedRoomId, String)>,
}
pub struct Saved {
    pub user: OwnedUserId,
    pub name: String,
    pub avatar: Option<OwnedMxcUri>,
    pub public: Public,
    pub changed_name: bool,
    pub changed_avatar: bool,
    pub room: Option<(OwnedRoomId, String)>,
}

pub async fn load(client: &Client, room_id: Option<&OwnedRoomId>) -> anyhow::Result<Details> {
    let account = client.account();
    let raw = match account.fetch_user_profile().await {
        Ok(raw) => raw,
        Err(error)
            if error.client_api_error_kind()
                == Some(&matrix_sdk::ruma::api::error::ErrorKind::NotFound) =>
        {
            matrix_sdk::ruma::api::client::profile::get_profile::v3::Response::new()
        }
        Err(error) => return Err(error.into()),
    };
    let user = client
        .user_id()
        .ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let capabilities = client.homeserver_capabilities();
    let allowed = capabilities
        .extended_profile_fields()
        .await
        .ok()
        .map(|capability| KEYS.map(|key| capability.can_set_field(&ProfileFieldName::from(key))))
        .unwrap_or([false; 4]);
    let name = raw
        .get("displayname")
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(user.as_str())
        .to_owned();
    let avatar = raw
        .get("avatar_url")
        .and_then(|url| url.as_str())
        .filter(|url| !url.is_empty())
        .map(OwnedMxcUri::from);
    let room = if let Some(room) = room_id
        .and_then(|id| client.get_room(id))
        .filter(|room| room.state() == RoomState::Joined)
    {
        let room_name = room
            .display_name()
            .await
            .map(|name| name.to_string())
            .unwrap_or_else(|_| room.room_id().to_string());
        room.get_member_no_sync(user)
            .await?
            .map(|member| RoomProfile {
                id: room.room_id().to_owned(),
                name: room_name,
                nickname: member.display_name().unwrap_or(&name).to_owned(),
            })
    } else {
        None
    };
    Ok(Details {
        public: Public::read(&raw),
        allowed,
        can_name: capabilities.can_change_displayname().await.unwrap_or(true),
        can_avatar: capabilities.can_change_avatar().await.unwrap_or(true),
        name,
        avatar,
        room,
    })
}

pub fn validate(
    fields: &[Option<String>; 4],
    name: Option<&str>,
    nickname: Option<&str>,
) -> anyhow::Result<()> {
    for (value, label) in [(name, "display name"), (nickname, "room nickname")] {
        if let Some(value) = value {
            anyhow::ensure!(
                !value.trim().is_empty() && value.chars().count() <= 255,
                "Enter a {label} between 1 and 255 characters."
            );
        }
    }
    for (index, value) in fields.iter().enumerate() {
        if let Some(value) = value {
            anyhow::ensure!(
                value.chars().count() <= LIMITS[index],
                "Enter a shorter {} (maximum {} characters).",
                LABELS[index].to_lowercase(),
                LIMITS[index]
            );
        }
    }
    if let Some(website) = &fields[2]
        && !website.is_empty()
    {
        let url = reqwest::Url::parse(website).map_err(|_| {
            anyhow::anyhow!("Enter a full website address starting with https:// or http://.")
        })?;
        anyhow::ensure!(
            (url.scheme() == "https" || url.scheme() == "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "Enter a public http:// or https:// website address without login details."
        );
    }
    Ok(())
}

pub async fn save(client: &Client, edit: Edit) -> anyhow::Result<Saved> {
    validate(
        &edit.fields,
        edit.name.as_deref(),
        edit.room.as_ref().map(|(_, name)| name.as_str()),
    )?;
    if let AvatarEdit::Upload { bytes, mime } = &edit.avatar {
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() <= UPLOAD_LIMIT && sniff(bytes).is_some(),
            "Choose a supported profile photo under 20 MB."
        );
        let mime: mime::Mime = mime.parse()?;
        anyhow::ensure!(
            mime.type_() == mime::IMAGE,
            "Choose an image for your profile photo."
        );
    }
    let current = load(client, edit.room.as_ref().map(|(id, _)| id)).await?;
    anyhow::ensure!(
        edit.name.is_none() || current.can_name,
        "Your server doesn't allow changing your display name."
    );
    anyhow::ensure!(
        matches!(&edit.avatar, AvatarEdit::Keep) || current.can_avatar,
        "Your server doesn't allow changing your profile photo."
    );
    for (index, value) in edit.fields.iter().enumerate() {
        anyhow::ensure!(
            value.is_none() || current.allowed[index],
            "Your server doesn't allow changing {}.",
            LABELS[index].to_lowercase()
        );
    }
    if let Some((id, _)) = &edit.room {
        anyhow::ensure!(
            current.room.as_ref().is_some_and(|room| &room.id == id),
            "Join this room before changing your nickname."
        );
    }
    let account = client.account();
    let mut public = current.public;
    // All permissions and values are checked before the first write. These API
    // calls are not transactional; report partial failure without discarding drafts.
    let result: anyhow::Result<Saved> = async {
        for (index, value) in edit.fields.into_iter().enumerate() {
            if let Some(value) = value
                && value != public.values[index]
            {
                if value.is_empty() {
                    if let Err(error) = account
                        .delete_profile_field(ProfileFieldName::from(KEYS[index]))
                        .await
                    {
                        if error.client_api_error_kind()
                            != Some(&matrix_sdk::ruma::api::error::ErrorKind::NotFound)
                        {
                            return Err(error.into());
                        }
                    }
                } else {
                    account
                        .set_profile_field(ProfileFieldValue::new(KEYS[index], json!(value))?)
                        .await?;
                }
                public.values[index] = value;
            }
        }
        let changed_avatar = !matches!(&edit.avatar, AvatarEdit::Keep);
        let avatar = match edit.avatar {
            AvatarEdit::Keep => current.avatar,
            AvatarEdit::Remove => {
                account.set_avatar_url(None).await?;
                None
            }
            AvatarEdit::Upload { bytes, mime } => {
                anyhow::ensure!(
                    bytes.len() <= UPLOAD_LIMIT,
                    "Choose a profile photo under 20 MB."
                );
                let mime: mime::Mime = mime.parse()?;
                Some(account.upload_avatar(&mime, bytes.to_vec()).await?)
            }
        };
        let changed_name = edit.name.as_ref().is_some_and(|name| name != &current.name);
        let name = edit.name.unwrap_or(current.name);
        if changed_name {
            account.set_display_name(Some(&name)).await?;
        }
        if let Some((id, nickname)) = &edit.room {
            use matrix_sdk::ruma::api::client::state::get_state_event_for_key;
            use matrix_sdk::ruma::events::{
                StateEventType,
                room::member::{MembershipState, RoomMemberEventContent},
            };
            let user = client
                .user_id()
                .ok_or_else(|| anyhow::anyhow!("not signed in"))?;
            let request = get_state_event_for_key::v3::Request::new(
                id.clone(),
                StateEventType::RoomMember,
                user.to_string(),
            );
            let response = client.send(request).await?;
            let mut content: RoomMemberEventContent =
                serde_json::from_str(response.event_or_content.get())?;
            anyhow::ensure!(
                content.membership == MembershipState::Join,
                "Join this room before changing your nickname."
            );
            content.displayname = Some(nickname.clone());
            if changed_avatar {
                content.avatar_url = avatar.clone();
            }
            client
                .get_room(id)
                .ok_or_else(|| anyhow::anyhow!("Room is no longer available."))?
                .send_state_event_for_key(user, content)
                .await?;
        }
        Ok(Saved {
            user: client
                .user_id()
                .ok_or_else(|| anyhow::anyhow!("not signed in"))?
                .to_owned(),
            name,
            avatar,
            public,
            changed_name,
            changed_avatar,
            room: edit.room,
        })
    }
    .await;
    result.map_err(|error| {
        anyhow::anyhow!(
            "Some profile changes may have been saved. Check your profile and try again. {error}"
        )
    })
}

pub struct Panel {
    pub details: Option<Box<Details>>,
    pub loading: bool,
    pub error: Option<String>,
    pub ticket: u64,
    pub fields: Vec<Entity<Input>>,
    pub nickname: Entity<Input>,
}
impl Panel {
    pub fn new(cx: &mut App) -> Self {
        Self {
            details: None,
            loading: false,
            error: None,
            ticket: 0,
            fields: LABELS
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    cx.new(|cx| {
                        let input = Input::new(*label, cx);
                        if i == 0 { input.growing() } else { input }
                    })
                })
                .collect(),
            nickname: cx.new(|cx| Input::new("Nickname in this room", cx)),
        }
    }
    pub fn begin(&mut self, cx: &mut App) {
        self.ticket = self.ticket.wrapping_add(1);
        self.details = None;
        self.loading = true;
        self.error = None;
        for input in self.fields.iter().chain(std::iter::once(&self.nickname)) {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
    }
    pub fn loaded(&mut self, details: Box<Details>, cx: &mut App) {
        for (input, value) in self.fields.iter().zip(&details.public.values) {
            input.update(cx, |input, cx| input.set_text(value.clone(), cx));
        }
        self.nickname.update(cx, |input, cx| {
            input.set_text(
                details
                    .room
                    .as_ref()
                    .map(|room| room.nickname.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.details = Some(details);
        self.loading = false;
    }
}

fn input_row(label: &'static str, input: &Entity<Input>, enabled: bool, cx: &App) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(cx.theme().text(Text::Small))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label),
        )
        .child(if enabled {
            div().min_h(px(40.)).child(input.clone()).into_any_element()
        } else {
            div()
                .min_h(px(28.))
                .text_color(cx.theme().muted_foreground)
                .child(input.read(cx).text().to_owned())
                .into_any_element()
        })
        .into_any_element()
}

impl MatrixClient {
    pub(super) fn open_person(&mut self, person: Person, cx: &mut Context<Self>) {
        self.command(Command::LoadPublicProfile(person.id.clone()));
        self.person = Some(person);
        cx.notify();
    }

    pub(super) fn public_profile_body(
        &self,
        user: &OwnedUserId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(profile) = self.public_profiles.get(user.as_str()) else {
            return div().into_any_element();
        };
        let mut body = div().flex().flex_col().gap_2();
        for (index, value) in profile
            .values
            .iter()
            .enumerate()
            .filter(|(_, value)| !value.is_empty())
        {
            let mut row = div().flex().flex_col().gap_1().child(
                div()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(LABELS[index]),
            );
            if index == 2
                && reqwest::Url::parse(value).is_ok_and(|url| {
                    (url.scheme() == "https" || url.scheme() == "http") && url.host_str().is_some()
                })
            {
                let url = value.clone();
                row = row.child(
                    Button::new("person-website")
                        .ghost()
                        .label(value.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            } else {
                row = row.child(div().child(value.clone()));
            }
            body = body.child(row);
        }
        body.into_any_element()
    }

    pub(super) fn save_profile(&mut self, cx: &mut Context<Self>) {
        if self.profile_busy || self.profile.loading {
            return;
        }
        let Some(details) = &self.profile.details else {
            self.notify("Load your profile before saving changes.", true, cx);
            return;
        };
        let name = self.profile_name.read(cx).text().trim().to_owned();
        let changed_name = name != details.name;
        let fields = std::array::from_fn(|index| {
            let value = self.profile.fields[index].read(cx).text().trim().to_owned();
            (details.allowed[index] && value != details.public.values[index]).then_some(value)
        });
        let room = details.room.as_ref().and_then(|room| {
            let nickname = self.profile.nickname.read(cx).text().trim().to_owned();
            let nickname = if nickname.is_empty() {
                name.clone()
            } else {
                nickname
            };
            (nickname != room.nickname || (changed_name && room.nickname != details.name))
                .then(|| (room.id.clone(), nickname))
        });
        if let Err(error) = validate(
            &fields,
            changed_name.then_some(name.as_str()),
            room.as_ref().map(|(_, nickname)| nickname.as_str()),
        ) {
            self.profile.error = Some(error.to_string());
            cx.notify();
            return;
        }
        let avatar = if let Some(photo) = &self.profile_photo {
            AvatarEdit::Upload {
                bytes: Arc::from(photo.bytes.as_slice()),
                mime: photo.mime.clone(),
            }
        } else if self.profile_remove {
            AvatarEdit::Remove
        } else {
            AvatarEdit::Keep
        };
        if !changed_name
            && fields.iter().all(Option::is_none)
            && room.is_none()
            && matches!(&avatar, AvatarEdit::Keep)
        {
            self.close_dialog(cx);
            return;
        }
        self.profile.error = None;
        self.profile_busy = true;
        self.command(Command::Profile(Edit {
            name: changed_name.then_some(name),
            avatar,
            fields,
            room,
        }));
        cx.notify();
    }

    pub(super) fn render_profile(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(me) = &self.me else {
            return div().into_any_element();
        };
        let details = self.profile.details.as_deref();
        let picture = details
            .map(|details| details.avatar.as_ref())
            .unwrap_or(me.avatar.as_ref());
        let busy = self.profile_busy || self.profile.loading;
        let can_name = details.is_some_and(|details| details.can_name);
        let can_avatar = details.is_some_and(|details| details.can_avatar);
        let face = if self.profile_remove {
            avatar(me.id.as_str(), &me.name, px(80.)).into_any_element()
        } else if let Some(photo) = &self.profile_photo {
            img(photo.preview.clone().expect("profile image preview"))
                .size(px(80.))
                .rounded_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            self.face(me.id.as_str(), &me.name, picture, px(80.))
        };
        let has_photo = self.profile_photo.is_some() || (picture.is_some() && !self.profile_remove);
        let photo_controls = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Button::new("profile-photo")
                    .label("Change photo")
                    .disabled(busy || !can_avatar)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_photo(cx))),
            )
            .when(has_photo, |body| {
                body.child(
                    Button::new("profile-remove")
                        .ghost()
                        .label("Remove photo")
                        .disabled(busy || !can_avatar)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.profile_photo = None;
                            this.profile_remove = true;
                            cx.notify();
                        })),
                )
            });
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .on_action(cx.listener(|this, _: &Submit, _, cx| this.save_profile(cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(face)
                    .child(photo_controls),
            )
            .child(input_row(
                "Display name",
                &self.profile_name,
                can_name && !busy,
                cx,
            ));
        if self.profile.loading {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Loading profile and available fields…"),
            );
        }
        if let Some(error) = &self.profile.error {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(error.clone()),
            );
        }
        if let Some(details) = details {
            body = body.child(div().font_weight(FontWeight::SEMIBOLD).mt_2().child("More about you"))
                .child(div().text_size(cx.theme().text(Text::Small)).text_color(cx.theme().muted_foreground)
                    .child("These details are public. Other Huddle users can see them on your profile. Other Matrix clients may not display every field."));
            for index in 0..4 {
                body = body
                    .child(input_row(
                        LABELS[index],
                        &self.profile.fields[index],
                        details.allowed[index] && !busy,
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(if !details.allowed[index] {
                                "This field is unavailable on your server.".to_owned()
                            } else if index == 0 {
                                "Up to 1000 characters. Shift+Enter adds a line. Leave blank to remove it.".into()
                            } else if index == 2 {
                                "Use a full https:// or http:// address. Leave blank to remove it."
                                    .into()
                            } else if index == 3 {
                                "For example: Europe/Warsaw or UTC. Leave blank to remove it."
                                    .into()
                            } else {
                                format!(
                                    "Up to {} characters. Leave blank to remove it.",
                                    LIMITS[index]
                                )
                            }),
                    );
            }
            if let Some(room) = &details.room {
                body = body.child(div().mt_2().font_weight(FontWeight::SEMIBOLD).child(format!("Nickname in {}", room.name)))
                    .child(div().text_size(cx.theme().text(Text::Small)).text_color(cx.theme().muted_foreground).child("Only changes your name in this room. Leave blank to use your display name."))
                    .child(input_row("Room nickname", &self.profile.nickname, !busy, cx))
                    .child(Button::new("profile-reset-nickname").ghost().label("Use display name").disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| { let name = this.profile_name.read(cx).text().to_owned(); this.profile.nickname.update(cx, |input, cx| input.set_text(name, cx)); cx.notify(); })));
            }
        }
        let user = me.id.to_string();
        let homeserver = me.id.server_name().to_string();
        body = body
            .child(
                div()
                    .mt_2()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Matrix ID: {user}\nHomeserver: {homeserver}")),
            )
            .child(
                Button::new("profile-copy-id")
                    .ghost()
                    .label("Copy Matrix ID")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(user.clone()))
                    }),
            );
        Modal::new("dialog-profile", "Edit profile")
            .w(px(560.))
            .detail("Choose how you appear in rooms and on your profile.")
            .close_button()
            .on_dismiss(cx.listener(|this, _: &(), _, cx| this.close_dialog(cx)))
            .child(body)
            .when(
                self.profile.details.is_none() && !self.profile.loading,
                |modal| {
                    modal.action(
                        Button::new("profile-reload")
                            .label("Retry loading")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.profile.loading = true;
                                this.profile.error = None;
                                this.profile.ticket = this.profile.ticket.wrapping_add(1);
                                this.command(Command::LoadProfile {
                                    room: this.room.clone(),
                                    ticket: this.profile.ticket,
                                });
                                cx.notify();
                            })),
                    )
                },
            )
            .action(
                Button::new("profile-cancel")
                    .ghost()
                    .label("Cancel")
                    .disabled(self.profile_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
            )
            .action(
                Button::new("profile-save")
                    .primary()
                    .label(if self.profile_busy {
                        "Saving…"
                    } else {
                        "Save"
                    })
                    .disabled(busy || self.profile.details.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
            )
            .into_any_element()
    }
}
