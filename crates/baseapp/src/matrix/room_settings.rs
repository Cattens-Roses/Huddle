//! Administration UI shared by rooms and spaces.
use super::*;
use backend::admin::{self, Action, MemberAction, Setting, Snapshot};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Access,
    Addresses,
    Permissions,
    Members,
    Rooms,
}
pub struct Panel {
    pub snapshot: Option<Box<Snapshot>>,
    pub loading: bool,
    pub status: Option<String>,
    pub confirm: Option<(Action, String)>,
    tab: Tab,
    target: Entity<Input>,
    role: Entity<Input>,
    reason: Entity<Input>,
    alias: Entity<Input>,
    allowed: Entity<Input>,
    child: Entity<Input>,
    new_room: Entity<Input>,
    order: Entity<Input>,
    suggested: bool,
    powers: Vec<Entity<Input>>,
    member_limit: usize,
}
impl Panel {
    pub fn new(cx: &mut App) -> Self {
        let mut input = |hint| cx.new(|cx| Input::new(hint, cx));
        Self {
            snapshot: None,
            loading: false,
            status: None,
            confirm: None,
            tab: Tab::General,
            target: input("@person:matrix.org"),
            role: input("Power level, e.g. 0, 50 or 100"),
            reason: input("Reason (optional)"),
            alias: input("#room:matrix.org"),
            allowed: input("!space:matrix.org"),
            child: input("!room:matrix.org"),
            new_room: input("Room name"),
            order: input("Sort order (optional)"),
            suggested: false,
            powers: admin::POWER_LABELS
                .iter()
                .map(|label| input(*label))
                .collect(),
            member_limit: 50,
        }
    }
    pub fn reset(&mut self) {
        self.snapshot = None;
        self.loading = false;
        self.status = None;
        self.confirm = None;
        self.tab = Tab::General;
        self.member_limit = 50;
    }
    pub fn begin(&mut self, cx: &mut App) {
        self.reset();
        for input in [
            &self.target,
            &self.role,
            &self.reason,
            &self.alias,
            &self.allowed,
            &self.child,
            &self.new_room,
            &self.order,
        ] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.suggested = false;
    }
    pub fn load(&mut self, snapshot: Box<Snapshot>, cx: &mut App) {
        self.alias
            .update(cx, |input, cx| input.set_text(snapshot.alias.clone(), cx));
        self.allowed.update(cx, |input, cx| {
            input.set_text(snapshot.allowed_spaces.clone(), cx)
        });
        for (input, power) in self.powers.iter().zip(snapshot.powers) {
            input.update(cx, |input, cx| input.set_text(power.to_string(), cx));
        }
        self.snapshot = Some(snapshot);
    }
}

fn section(title: &'static str, detail: &'static str, cx: &App) -> Div {
    let theme = *cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .when(!detail.is_empty(), |body| {
            body.child(
                div()
                    .text_size(theme.text(Text::Small))
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
        })
}
fn field(title: &'static str, input: &Entity<Input>, enabled: bool, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(cx.theme().text(Text::Small)).child(title))
        .child(if enabled {
            div().min_h(px(40.)).child(input.clone()).into_any_element()
        } else {
            div()
                .text_color(cx.theme().muted_foreground)
                .child(input.read(cx).text().to_owned())
                .into_any_element()
        })
}

impl MatrixClient {
    fn perform_admin(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.room_settings_busy || self.admin.loading {
            return;
        }
        let Some(room) = self.settings_room.clone() else {
            return;
        };
        self.admin.confirm = None;
        self.admin.status = None;
        self.room_settings_busy = true;
        self.command(Command::RoomAdmin { room, action });
        cx.notify();
    }
    fn admin_button(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        action: Action,
        confirmation: Option<String>,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .disabled(!enabled || self.room_settings_busy || self.admin.loading)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(message) = &confirmation {
                    this.admin.confirm = Some((action.clone(), message.clone()));
                    cx.notify();
                } else {
                    this.perform_admin(action.clone(), cx);
                }
            }))
    }
    fn admin_choice(
        &self,
        key: &'static str,
        current: &str,
        choices: &[(&'static str, &'static str)],
        enabled: bool,
        setting: Setting,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(choices.iter().map(|&(value, label)| {
                let value = value.to_owned();
                let confirmation = if value == "world_readable" {
                    Some(
                        "Anyone will be able to read this room's history. Apply this change?"
                            .to_owned(),
                    )
                } else if matches!(setting, Setting::Directory) && value == "public" {
                    Some("List this room in the public room directory?".to_owned())
                } else {
                    None
                };
                Button::new(SharedString::from(format!("{key}-{value}")))
                    .small()
                    .ghost()
                    .label(label)
                    .selected(value == current)
                    .disabled(
                        !enabled
                            || self.room_settings_busy
                            || self.admin.loading
                            || value == current,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let action = Action::Setting(setting, value.clone());
                        if let Some(message) = &confirmation {
                            this.admin.confirm = Some((action, message.clone()));
                            cx.notify();
                        } else {
                            this.perform_admin(action, cx);
                        }
                    }))
            }))
    }

    fn pick_room_avatar(&mut self, cx: &mut Context<Self>) {
        if self.room_settings_busy || self.admin.loading {
            return;
        }
        let expected = self.settings_room.clone();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a room icon".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_spawn(async move {
                    use std::io::Read as _;
                    let file =
                        std::fs::File::open(path).map_err(|_| "That file couldn't be opened.")?;
                    let mut bytes = Vec::new();
                    file.take(UPLOAD_LIMIT as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "That file couldn't be read.")?;
                    if bytes.len() > UPLOAD_LIMIT {
                        return Err("Choose an icon under 20 MB.");
                    }
                    let format = sniff(&bytes).ok_or("Choose a PNG, JPEG, GIF or WebP image.")?;
                    Ok((bytes, mime_of(format).to_owned()))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.settings_room != expected || this.dialog != Some(Dialog::RoomSettings) {
                    return;
                }
                match result {
                    Ok((bytes, mime)) => this.perform_admin(
                        Action::Avatar {
                            bytes: bytes.into(),
                            mime,
                        },
                        cx,
                    ),
                    Err(error) => {
                        this.admin.status = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn admin_member_action(&mut self, kind: MemberAction, cx: &mut Context<Self>) {
        let target = self.admin.target.read(cx).text().trim().to_owned();
        let Ok(user) = OwnedUserId::try_from(target.as_str()) else {
            self.admin.status =
                Some("Enter a full Matrix user ID, such as @person:matrix.org.".into());
            cx.notify();
            return;
        };
        let reason = self.admin.reason.read(cx).text().trim().to_owned();
        let message = match kind {
            MemberAction::Invite => None,
            MemberAction::Kick => Some(format!("Remove {user} from this room?")),
            MemberAction::Ban => Some(format!(
                "Ban {user}? They will not be able to rejoin until unbanned."
            )),
            MemberAction::Unban => Some(format!(
                "Allow {user} to join again? This does not invite them."
            )),
            MemberAction::Power(value) => Some(format!(
                "Set {user}'s power level to {value}? Levels 50 and 100 usually grant moderator and administrator access."
            )),
        };
        let action = Action::Member {
            user,
            action: kind,
            reason,
        };
        if let Some(message) = message {
            self.admin.confirm = Some((action, message));
            cx.notify();
        } else {
            self.perform_admin(action, cx);
        }
    }

    pub(super) fn render_room_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some((action, message)) = &self.admin.confirm {
            let action = action.clone();
            return Modal::new("dialog-room-settings-confirm", "Confirm change")
                .w(px(480.))
                .detail(message.clone())
                .close_button()
                .on_dismiss(cx.listener(|this, _: &(), _, cx| {
                    this.admin.confirm = None;
                    cx.notify();
                }))
                .action(
                    Button::new("admin-confirm-cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.admin.confirm = None;
                            cx.notify();
                        })),
                )
                .action(
                    Button::new("admin-confirm")
                        .primary()
                        .label("Confirm")
                        .disabled(self.room_settings_busy || self.admin.loading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.perform_admin(action.clone(), cx)
                        })),
                )
                .into_any_element();
        }
        let busy = self.room_settings_busy || self.admin.loading;
        let snapshot = self.admin.snapshot.as_deref();
        let room = self.settings_room.as_ref().and_then(|id| self.summary(id));
        let space = snapshot
            .map(|snapshot| snapshot.space)
            .unwrap_or_else(|| room.is_some_and(|room| room.kind == RoomKind::Space));
        let mut body = div().flex().flex_col().gap_3();
        let tabs = [
            (Tab::General, "General"),
            (Tab::Access, "Access"),
            (Tab::Addresses, "Addresses"),
            (Tab::Permissions, "Permissions"),
            (Tab::Members, "Members"),
            (Tab::Rooms, "Rooms & spaces"),
        ];
        body = body.child(
            div().flex().flex_wrap().gap_1().children(
                tabs.into_iter()
                    .filter(|(tab, _)| *tab != Tab::Rooms || space)
                    .map(|(tab, label)| {
                        Button::new(SharedString::from(format!("admin-tab-{label}")))
                            .ghost()
                            .small()
                            .label(label)
                            .selected(self.admin.tab == tab)
                            .disabled(self.room_settings_busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.admin.tab = tab;
                                this.admin.confirm = None;
                                cx.notify();
                            }))
                    }),
            ),
        );
        if self.admin.loading {
            body = body.child(div().child("Loading current room settings…"));
        }
        if let Some(status) = &self.admin.status {
            body = body.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(status.clone()),
            );
        }
        if self.admin.tab == Tab::General {
            let can_name = room.is_some_and(|room| room.can_edit_name);
            let can_topic = room.is_some_and(|room| room.can_edit_topic);
            body = body.child(
                section(
                    "Room details",
                    "Leave the name blank to use the automatic room name.",
                    cx,
                )
                .child(field("Name", &self.room_name, can_name && !busy, cx))
                .child(field("Topic", &self.room_topic, can_topic && !busy, cx))
                .child(
                    Button::new("admin-save-details")
                        .primary()
                        .label("Save details")
                        .disabled(busy || (!can_name && !can_topic))
                        .on_click(cx.listener(|this, _, _, cx| this.save_room_settings(cx))),
                ),
            );
            if let Some(s) = snapshot {
                body = body.child(
                    section("Room icon", "Used in the room list and room header.", cx)
                        .when_some(s.avatar.as_ref(), |body, avatar| {
                            body.child(self.face("admin-room-icon", "", Some(avatar), px(64.)))
                        })
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("admin-upload-avatar")
                                        .label("Choose image")
                                        .disabled(busy || !s.permissions.avatar)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.pick_room_avatar(cx)),
                                        ),
                                )
                                .child(self.admin_button(
                                    "admin-remove-avatar",
                                    "Remove icon",
                                    s.permissions.avatar && s.avatar.is_some(),
                                    Action::RemoveAvatar,
                                    Some("Remove the room icon?".into()),
                                    cx,
                                )),
                        ),
                );
                body = body.child(
                    section(
                        "Room information",
                        "Room IDs and room versions are assigned when a room is created.",
                        cx,
                    )
                    .child(div().child(format!(
                            "Room ID: {}",
                            self.settings_room
                                .as_ref()
                                .map(|id| id.as_str())
                                .unwrap_or_default()
                        )))
                    .child(div().child(format!("Room version: {}", s.version))),
                );
            }
        } else if let Some(s) = snapshot {
            let p = &s.permissions;
            match self.admin.tab {
                Tab::Access => {
                    let choices = [
                        ("invite", "Invite only"),
                        ("public", "Anyone"),
                        ("knock", "Request to join"),
                        ("restricted", "Members of spaces"),
                        ("knock_restricted", "Spaces or request"),
                    ];
                    body = body.child(section("Who can join", "Restricted access allows members of the listed spaces to join.", cx)
                        .child(div().child(format!("Current rule: {}", s.join)))
                        .child(field("Allowed space IDs, separated by spaces", &self.admin.allowed, p.join && !busy, cx))
                        .child(div().flex().flex_wrap().gap_2().children(choices.into_iter().map(|(value, label)| {
                            Button::new(SharedString::from(format!("join-{value}"))).ghost().small().label(label).selected(s.join == value).disabled(busy || !p.join)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let text = this.admin.allowed.read(cx).text().to_owned();
                                    let spaces = if value == "restricted" || value == "knock_restricted" {
                                        match text.split_whitespace().map(OwnedRoomId::try_from).collect::<Result<Vec<_>, _>>() {
                                            Ok(ids) if !ids.is_empty() => ids,
                                            _ => { this.admin.status = Some("Enter at least one valid space ID (!space:server).".into()); cx.notify(); return; }
                                        }
                                    } else { Vec::new() };
                                    let action = Action::Join { rule: value.into(), spaces };
                                    this.admin.confirm = Some((action, format!("Change who can join to '{label}'? Existing members stay joined."))); cx.notify();
                                }))
                        }))));
                    body = body.child(
                        section(
                            "Message history",
                            "Choose which messages people are allowed to read.",
                            cx,
                        )
                        .child(self.admin_choice(
                            "history",
                            &s.history,
                            &[
                                ("joined", "Since joining"),
                                ("invited", "Since invitation"),
                                ("shared", "Members"),
                                ("world_readable", "Anyone"),
                            ],
                            p.history,
                            Setting::History,
                            cx,
                        )),
                    );
                    body = body.child(
                        section(
                            "Guest accounts",
                            "Your homeserver must also allow guest access.",
                            cx,
                        )
                        .child(self.admin_choice(
                            "guests",
                            &s.guest,
                            &[("forbidden", "Not allowed"), ("can_join", "Allowed")],
                            p.guest,
                            Setting::Guest,
                            cx,
                        )),
                    );
                    body = body.child(
                        section(
                            "Public directory",
                            "Listing a room does not change who can join it.",
                            cx,
                        )
                        .child(div().child(format!(
                            "Current visibility: {}",
                            s.directory.as_deref().unwrap_or("Unavailable")
                        )))
                        .child(self.admin_choice(
                            "directory",
                            s.directory.as_deref().unwrap_or_default(),
                            &[("private", "Unlisted"), ("public", "Listed")],
                            p.join,
                            Setting::Directory,
                            cx,
                        )),
                    );
                    body = body.child(section("Encryption", if space { "Encryption is configured separately for each room in a space." } else { "Once encryption is enabled, it cannot be disabled for this room." }, cx)
                        .child(div().child(if s.encrypted { "Encryption is enabled." } else { "Encryption is not enabled." }))
                        .when(!space && !s.encrypted, |body| body.child(self.admin_button("admin-encryption", "Enable encryption", p.encryption, Action::EnableEncryption, Some("Enable end-to-end encryption? This cannot be undone. Existing messages are not encrypted retroactively.".into()), cx))));
                }
                Tab::Addresses => {
                    body = body.child(section("Room addresses", "Use a full address on a server where you can create aliases. Setting a new primary address keeps the previous address as an alternative.", cx)
                        .child(div().child(format!("Primary address: {}", if s.alias.is_empty() { "None" } else { &s.alias })))
                        .children(s.aliases.iter().map(|alias| div().child(alias.clone())))
                        .child(field("Address", &self.admin.alias, p.alias && !busy, cx))
                        .child(div().flex().gap_2().children([(false, "Set primary"), (true, "Remove address")].into_iter().map(|(remove, label)| {
                            Button::new(SharedString::from(format!("alias-{remove}"))).label(label).disabled(busy || !p.alias)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let alias = this.admin.alias.read(cx).text().trim().to_owned();
                                    this.admin.confirm = Some((Action::Alias { alias: alias.clone(), remove }, format!("{label}: {alias}?"))); cx.notify();
                                }))
                        }))));
                }
                Tab::Permissions => {
                    body = body.child(section("Default permissions", "Power levels: member 0, moderator 50, administrator 100. Existing user roles and event-specific overrides are preserved.", cx)
                        .children(admin::POWER_LABELS.iter().zip(&self.admin.powers).map(|(&title, input)| field(title, input, p.power && !busy, cx)))
                        .child(Button::new("admin-save-powers").primary().label("Apply permissions").disabled(busy || !p.power)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let values = this.admin.powers.iter().map(|input| input.read(cx).text().trim().parse::<i64>()).collect::<Result<Vec<_>, _>>();
                                match values.and_then(|values| Ok(values.try_into().unwrap())) {
                                    Ok(values) => this.admin.confirm = Some((Action::Powers(values), "Apply these permission levels? This can change who may send messages or administer the room.".into())),
                                    Err(_) => this.admin.status = Some("Enter a whole number for every permission level.".into()),
                                }
                                cx.notify();
                            }))));
                }
                Tab::Members => {
                    let mut actions = div().flex().flex_wrap().gap_2();
                    for (label, kind, enabled) in [
                        ("Invite", MemberAction::Invite, p.invite),
                        ("Remove", MemberAction::Kick, p.kick),
                        ("Ban", MemberAction::Ban, p.ban),
                        ("Unban", MemberAction::Unban, p.ban),
                    ] {
                        actions = actions.child(
                            Button::new(SharedString::from(format!("member-{label}")))
                                .label(label)
                                .disabled(busy || !enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.admin_member_action(kind.clone(), cx)
                                })),
                        );
                    }
                    body = body.child(section("Manage a member", "Select a person below or enter their full Matrix ID. Moderation respects the target's role.", cx)
                        .child(field("User", &self.admin.target, !busy, cx)).child(field("Reason", &self.admin.reason, !busy, cx)).child(actions)
                        .child(field("Power level", &self.admin.role, p.power && !busy, cx))
                        .child(Button::new("admin-member-role").label("Update role").disabled(busy || !p.power)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let level = this.admin.role.read(cx).text().trim().parse::<i64>();
                                match level { Ok(level) => this.admin_member_action(MemberAction::Power(level), cx), Err(_) => { this.admin.status = Some("Enter a numeric power level, such as 0, 50 or 100.".into()); cx.notify(); } }
                            }))));
                    body =
                        body.child(
                            section(
                                "Members and bans",
                                "Members, invitations, join requests and bans.",
                                cx,
                            )
                            .children(s.members.iter().take(self.admin.member_limit).map(
                                |member| {
                                    let id = member.id.clone();
                                    let power = member.power.clone();
                                    Button::new(SharedString::from(format!("admin-member-{id}")))
                                        .ghost()
                                        .w_full()
                                        .label(format!(
                                            "{} · {} · {} · level {}",
                                            member.name, member.id, member.membership, member.power
                                        ))
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.admin.target.update(cx, |input, cx| {
                                                input.set_text(id.to_string(), cx)
                                            });
                                            this.admin.role.update(cx, |input, cx| {
                                                input.set_text(
                                                    if power == "Creator" {
                                                        String::new()
                                                    } else {
                                                        power.clone()
                                                    },
                                                    cx,
                                                )
                                            });
                                            focus(&this.admin.target, window, cx);
                                            cx.notify();
                                        }))
                                },
                            ))
                            .when(
                                s.members.len() > self.admin.member_limit,
                                |body| {
                                    body.child(
                                        Button::new("admin-more-members")
                                            .label("Show more members")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.admin.member_limit += 50;
                                                cx.notify();
                                            })),
                                    )
                                },
                            ),
                        );
                }
                Tab::Rooms => {
                    let available_spaces = self
                        .rooms
                        .iter()
                        .filter(|candidate| {
                            candidate.kind == RoomKind::Space
                                && !candidate.invite
                                && self.settings_room.as_ref() != Some(&candidate.id)
                                && !s.children.iter().any(|child| child.id == candidate.id)
                        })
                        .collect::<Vec<_>>();
                    if !available_spaces.is_empty() {
                        body = body.child(
                            section(
                                "Add an existing space",
                                "Select a space to nest inside this one.",
                                cx,
                            )
                            .children(
                                available_spaces.into_iter().map(|space| {
                                    let id = space.id.clone();
                                    let name = space.name.clone();
                                    Button::new(SharedString::from(format!("admin-add-space-{id}")))
                                        .ghost()
                                        .w_full()
                                        .icon("icons/folder-plus.svg")
                                        .label(name.clone())
                                        .disabled(busy || !p.children)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.admin.confirm = Some((
                                                Action::Child {
                                                    id: id.clone(),
                                                    order: String::new(),
                                                    suggested: false,
                                                    remove: false,
                                                },
                                                format!("Add '{name}' as a nested space?"),
                                            ));
                                            cx.notify();
                                        }))
                                }),
                            ),
                        );
                    }
                    body = body.child(
                        section("Create a room", "Create a new room inside this space.", cx)
                            .child(field("Name", &self.admin.new_room, p.children && !busy, cx))
                            .child(
                                Button::new("admin-create-room")
                                    .primary()
                                    .label("Create room")
                                    .disabled(busy || !p.children)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let name =
                                            this.admin.new_room.read(cx).text().trim().to_owned();
                                        if name.is_empty() {
                                            this.admin.status =
                                                Some("Enter a name for the new room.".into());
                                            cx.notify();
                                            return;
                                        }
                                        this.admin.confirm = Some((
                                            Action::CreateRoom(name.clone()),
                                            format!("Create '{name}' in this space?"),
                                        ));
                                        cx.notify();
                                    })),
                            ),
                    );
                    body = body.child(section("Rooms and spaces", "Add a room or nest another space using its Matrix ID. Removing a link keeps its membership unchanged.", cx)
                        .children(s.children.iter().map(|child| {
                            let id = child.id.clone(); let order = child.order.clone(); let suggested = child.suggested;
                            Button::new(SharedString::from(format!("admin-child-{id}"))).ghost().w_full().label(format!("{} · {}{}", self.summary(&id).map(|r| r.name.as_str()).unwrap_or(id.as_str()), order, if suggested { " · suggested" } else { "" })).disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| { this.admin.child.update(cx, |input, cx| input.set_text(id.to_string(), cx)); this.admin.order.update(cx, |input, cx| input.set_text(order.clone(), cx)); this.admin.suggested = suggested; cx.notify(); }))
                        }))
                        .child(field("Room or space ID", &self.admin.child, p.children && !busy, cx))
                        .child(field("Sort order", &self.admin.order, p.children && !busy, cx))
                        .child(Button::new("admin-child-suggested").ghost().label("Suggested").selected(self.admin.suggested).disabled(busy || !p.children)
                            .on_click(cx.listener(|this, _, _, cx| { this.admin.suggested = !this.admin.suggested; cx.notify(); })))
                        .child(div().flex().gap_2().children([(false, "Add or update link"), (true, "Remove link")].into_iter().map(|(remove, label)| {
                            Button::new(SharedString::from(format!("admin-child-action-{remove}"))).label(label).disabled(busy || !p.children)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let target = this.admin.child.read(cx).text().trim().to_owned();
                                    let Ok(id) = OwnedRoomId::try_from(target.as_str()) else { this.admin.status = Some("Enter a full room or space ID (!id:server).".into()); cx.notify(); return; };
                                    let order = this.admin.order.read(cx).text().to_owned();
                                    this.admin.confirm = Some((Action::Child { id, order, suggested: this.admin.suggested, remove }, format!("{label}? Membership of the linked room will not change."))); cx.notify();
                                }))
                        }))));
                }
                Tab::General => {}
            }
        } else if !self.admin.loading {
            body =
                body.child(div().child("Room settings could not be loaded. Refresh to try again."));
        }
        Modal::new(
            "dialog-room-settings",
            if space {
                "Space settings"
            } else {
                "Room settings"
            },
        )
        .w(px(720.))
        .close_button()
        .on_dismiss(cx.listener(|this, _: &(), _, cx| this.close_dialog(cx)))
        .child(body)
        .action(
            Button::new("admin-refresh")
                .ghost()
                .label("Refresh")
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(id) = this.settings_room.clone() {
                        this.admin.loading = true;
                        this.command(Command::LoadRoomAdmin(id));
                        cx.notify();
                    }
                })),
        )
        .action(
            Button::new("admin-close")
                .label("Close")
                .disabled(self.room_settings_busy)
                .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx))),
        )
        .into_any_element()
    }
}
