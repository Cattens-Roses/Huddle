mod backend;
mod download_ui;
mod media_ui;
mod media_zoom;
mod photo;
mod playback;
mod profile;
mod rail_groups;
mod receipts;
mod rich;
mod room_settings;
mod spaces;
mod text_selection;
mod typing;
mod unread;
mod upload;

use crate::cache::Cache;
use crate::notifications::NotificationPreferences;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, TimeZone as _};
use futures::StreamExt as _;
use gpui::prelude::*;
use gpui::{
    Anchor, AnimationExt as _, AnyElement, App, ClickEvent, ClipboardItem, Context, Div, ElementId,
    Entity, EventEmitter, ExternalPaths, FontWeight, Hsla, Image, ImageFormat, ListAlignment,
    ListState, MouseButton, ObjectFit, Pixels, SharedString, SpringAnimation, Task, WeakEntity,
    Window, anchored, deferred, div, img, list, point, px, svg,
};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::{EventId, OwnedEventId, OwnedMxcUri, OwnedRoomId, OwnedUserId};
use tokio::sync::mpsc::UnboundedSender;
use ui::{
    ActiveTheme as _, Button, Checkbox, Copy, Dismiss, Fading as _, Input, Modal, Motioned as _,
    PastedPicture, Perch, Scrollbar, Scroller, Scrubber, ScrubberState, Submit, Text, Tipped as _,
    Toast,
};

use backend::{
    Answer, Command, Event, Member, Message, MessageKind, Picture, Reaction, Recovery, RoomKind,
    RoomSummary, Step, Update, Verify,
};
use rich::OnLink;

pub use photo::PhotoSize;

const RAIL: Pixels = px(72.);
const CHANNELS: Pixels = px(240.);
const MEMBERS: Pixels = px(232.);
const THREAD_PANEL: Pixels = px(380.);
const AVATAR: Pixels = px(40.);
const GROUP_GAP: i64 = 7 * 60 * 1000;
const NOTICE: Duration = Duration::from_secs(5);
const ORPHANS: usize = 500;
const SUGGESTED: [(&str, &str, &str); 3] = [
    (
        "Matrix HQ",
        "#matrix:matrix.org",
        "News and chat about Matrix itself",
    ),
    (
        "Matrix Community",
        "#community:matrix.org",
        "Meet other people who use Matrix",
    ),
    (
        "Rust",
        "#rust:matrix.org",
        "Chat with the Rust programming community",
    ),
];
const EMOJI: [&str; 12] = [
    "👍", "❤️", "😂", "😮", "😢", "🎉", "🔥", "👀", "🙏", "✅", "👎", "🚀",
];

/// Asks the app shell to show its appearance settings.
pub struct OpenSettings;
pub struct CacheChanged;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    SignedOut,
    SigningIn,
    SignedIn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialog {
    RoomSettings,
    Choose,
    Join,
    Create,
    Talk,
    Leave,
    RemoveSpace,
    Security,
    Profile,
}

/// What the security dialog is showing.
#[derive(Clone, PartialEq)]
enum Guard {
    Menu,
    Key,
    Recovering,
    Waiting,
    Incoming,
    Emojis(Vec<(String, String)>),
    Done,
    Failed(String),
    NewKey(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot {
    Main,
    Thread,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Draft,
    Step,
}

#[derive(Clone)]
enum Media {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

struct Me {
    id: OwnedUserId,
    name: String,
    avatar: Option<OwnedMxcUri>,
}

struct Upload {
    name: String,
    bytes: Vec<u8>,
    mime: String,
    width: u32,
    height: u32,
    preview: Option<Arc<Image>>,
    duration: Option<Duration>,
    thumbnail: Option<playback::Thumbnail>,
}

const UPLOAD_LIMIT: usize = 20 * 1024 * 1024;

#[derive(Clone)]
struct Target {
    id: OwnedEventId,
    sender: OwnedUserId,
    name: String,
}

#[derive(Clone, PartialEq, Eq)]
enum Place {
    Main(usize),
    Thread(OwnedEventId, usize),
}

enum Change {
    Updated(Place),
    Removed(Place, OwnedEventId),
}

enum Relate {
    Changed(Change),
    Orphan(Event),
    Nothing,
}

enum Insert {
    Appended(usize),
    Replaced(usize),
    Duplicate,
}

#[derive(Default)]
struct Timeline {
    receipts: receipts::ReadState,
    messages: Vec<Message>,
    threads: HashMap<OwnedEventId, Vec<Message>>,
    orphans: Vec<Event>,
    loaded: bool,
    token: Option<String>,
    loading: bool,
}

impl Timeline {
    fn locate(&self, id: &EventId) -> Option<Place> {
        if let Some(index) = self
            .messages
            .iter()
            .position(|message| message.id.as_deref() == Some(id))
        {
            return Some(Place::Main(index));
        }
        self.threads.iter().find_map(|(root, replies)| {
            replies
                .iter()
                .position(|message| message.id.as_deref() == Some(id))
                .map(|index| Place::Thread(root.clone(), index))
        })
    }

    fn find(&self, id: &EventId) -> Option<&Message> {
        match self.locate(id)? {
            Place::Main(index) => self.messages.get(index),
            Place::Thread(root, index) => self.threads.get(&root)?.get(index),
        }
    }

    fn at(&mut self, place: &Place) -> Option<&mut Message> {
        match place {
            Place::Main(index) => self.messages.get_mut(*index),
            Place::Thread(root, index) => self.threads.get_mut(root)?.get_mut(*index),
        }
    }

    /// Applies an edit, reaction or deletion to the message it points at.
    fn relate(&mut self, event: Event) -> Relate {
        let target = match &event {
            Event::Edit { target, .. } | Event::React { target, .. } => target.clone(),
            Event::Redact(id) => id.clone(),
            Event::Message(_) => return Relate::Nothing,
        };
        let place = self.locate(&target);
        match (event, place) {
            (Event::Redact(id), Some(place)) => {
                match &place {
                    Place::Main(index) => {
                        self.messages.remove(*index);
                    }
                    Place::Thread(root, index) => {
                        if let Some(replies) = self.threads.get_mut(root) {
                            replies.remove(*index);
                        }
                    }
                }
                Relate::Changed(Change::Removed(place, id))
            }
            (Event::Redact(id), None) => self.unreact(&id),
            (event, None) => Relate::Orphan(event),
            (
                Event::Edit {
                    sender, body, rich, ..
                },
                Some(place),
            ) => {
                let Some(message) = self.at(&place) else {
                    return Relate::Nothing;
                };
                if message.sender != sender {
                    return Relate::Nothing;
                }
                message.body = body;
                message.rich = rich;
                message.edited = true;
                Relate::Changed(Change::Updated(place))
            }
            (
                Event::React {
                    key, sender, id, ..
                },
                Some(place),
            ) => {
                let Some(message) = self.at(&place) else {
                    return Relate::Nothing;
                };
                if message
                    .reactions
                    .iter()
                    .any(|reaction| reaction.senders.iter().any(|(_, known)| known == &id))
                {
                    return Relate::Nothing;
                }
                match message
                    .reactions
                    .iter_mut()
                    .find(|reaction| reaction.key == key)
                {
                    Some(reaction) => reaction.senders.push((sender, id)),
                    None => message.reactions.push(Reaction {
                        key,
                        senders: vec![(sender, id)],
                    }),
                }
                Relate::Changed(Change::Updated(place))
            }
            (Event::Message(_), _) => Relate::Nothing,
        }
    }

    fn unreact(&mut self, id: &EventId) -> Relate {
        let strip = |message: &mut Message| {
            let before = message
                .reactions
                .iter()
                .map(|r| r.senders.len())
                .sum::<usize>();
            for reaction in &mut message.reactions {
                reaction.senders.retain(|(_, known)| known != id);
            }
            message
                .reactions
                .retain(|reaction| !reaction.senders.is_empty());
            before
                != message
                    .reactions
                    .iter()
                    .map(|r| r.senders.len())
                    .sum::<usize>()
        };
        for (index, message) in self.messages.iter_mut().enumerate() {
            if strip(message) {
                return Relate::Changed(Change::Updated(Place::Main(index)));
            }
        }
        for (root, replies) in &mut self.threads {
            for (index, message) in replies.iter_mut().enumerate() {
                if strip(message) {
                    return Relate::Changed(Change::Updated(Place::Thread(root.clone(), index)));
                }
            }
        }
        Relate::Nothing
    }

    fn keep(&mut self, event: Event) {
        self.orphans.push(event);
        if self.orphans.len() > ORPHANS {
            self.orphans.remove(0);
        }
    }

    /// Retries edits and reactions whose message wasn't loaded when they arrived.
    fn retry(&mut self) {
        for event in std::mem::take(&mut self.orphans) {
            if let Relate::Orphan(event) = self.relate(event) {
                self.orphans.push(event);
            }
        }
    }
}

fn insert(messages: &mut Vec<Message>, message: Message, mine: bool) -> Insert {
    if messages
        .iter()
        .any(|known| known.id.is_some() && known.id == message.id)
    {
        return Insert::Duplicate;
    }
    let pending = mine
        .then(|| {
            messages
                .iter()
                .position(|known| known.id.is_none() && known.body == message.body)
        })
        .flatten();
    match pending {
        Some(index) => {
            messages[index] = message;
            Insert::Replaced(index)
        }
        None => {
            messages.push(message);
            Insert::Appended(messages.len() - 1)
        }
    }
}

#[derive(Clone)]
struct View {
    picture: Picture,
    name: String,
}

#[derive(Clone)]
struct Person {
    id: OwnedUserId,
    name: String,
    avatar: Option<OwnedMxcUri>,
}

#[derive(Clone)]
enum Line {
    Ringing {
        room: OwnedRoomId,
        event: OwnedEventId,
        name: String,
    },
    Live {
        room: OwnedRoomId,
        name: String,
    },
}

pub struct MatrixClient {
    commands: UnboundedSender<Command>,
    phase: Phase,
    sign_in_error: Option<SharedString>,
    choosing_server: bool,
    signing_up: bool,
    step: Option<Step>,
    signup_busy: bool,
    agreed: bool,
    me: Option<Me>,
    rooms: Vec<RoomSummary>,
    space: Option<OwnedRoomId>,
    space_trail: spaces::Trail,
    space_transition: u64,
    space_backwards: bool,
    linked_names: HashMap<OwnedRoomId, String>,
    linked_name_requests: HashMap<OwnedRoomId, Instant>,
    removing_space: Option<(OwnedRoomId, String)>,
    room: Option<OwnedRoomId>,
    rail_groups: rail_groups::Layout,
    rail_hover: Option<String>,
    focused: bool,
    in_tray: bool,
    typing: typing::Remote,
    typing_local: typing::Local,
    typing_idle: Option<Task<()>>,
    typing_expiry: Option<Task<()>>,
    notifications: NotificationPreferences,
    timelines: HashMap<OwnedRoomId, Timeline>,
    members: HashMap<OwnedRoomId, Vec<Member>>,
    show_members: bool,
    list: ListState,
    history_anchor: Option<(OwnedRoomId, gpui::ListOffset, Pixels)>,
    chat_scroll: Entity<Scrollbar>,
    thread: Option<OwnedEventId>,
    thread_list: ListState,
    thread_scroll: Entity<Scrollbar>,
    dialog: Option<Dialog>,
    guard: Guard,
    security: Option<(bool, Recovery)>,
    banner_hidden: bool,
    notice: Option<(SharedString, bool)>,
    notice_timer: Option<Task<()>>,
    refresh: Option<Task<()>>,
    next_local: u64,
    media: RefCell<Cache<Media>>,
    retired_images: Rc<RefCell<Vec<Arc<Image>>>>,
    files: Cache<Arc<[u8]>>,
    cache_clearing: bool,
    cache_status: Option<String>,
    viewing: Option<View>,
    media_zoom: media_zoom::Zoom,
    download_jobs: Vec<download_ui::Job>,
    download_ticket: u64,
    media_menu: Option<(Picture, gpui::Point<Pixels>)>,
    player_media: Option<Picture>,
    player: Option<playback::Player>,
    player_audio: Option<String>,
    audio_thread_timeline: ScrubberState,
    player_image: Option<Arc<gpui::RenderImage>>,
    painted_player_image: Option<Arc<gpui::RenderImage>>,
    player_timeline: ScrubberState,
    player_volume: ScrubberState,
    player_scrub: Option<Duration>,
    pending_play: Option<Picture>,
    person: Option<Person>,
    line: Option<Line>,
    replying: Option<Target>,
    editing: Option<(OwnedEventId, Spot)>,
    picker: Option<(OwnedEventId, Spot)>,
    pending_focus: Option<Focus>,
    attachment: Option<(Spot, Upload)>,
    photo_compress: bool,
    photo_size: PhotoSize,
    compressing: Option<Spot>,
    attach_ticket: u64,
    profile_photo: Option<Upload>,
    profile_remove: bool,
    profile_busy: bool,
    server: Entity<Input>,
    username: Entity<Input>,
    password: Entity<Input>,
    confirm_password: Entity<Input>,
    step_field: Entity<Input>,
    draft: Entity<Input>,
    thread_draft: Entity<Input>,
    field: Entity<Input>,
    profile_name: Entity<Input>,
    profile: profile::Panel,
    public_profiles: Cache<profile::Public>,
    person_loading: bool,
    person_ticket: u64,
    shared_room_limit: usize,
    settings_room: Option<OwnedRoomId>,
    room_name: Entity<Input>,
    room_topic: Entity<Input>,
    room_settings_busy: bool,
    readers_view: Option<receipts::Selection>,
    own_read_targets: [Option<OwnedEventId>; 2],
    admin: room_settings::Panel,
    selection: text_selection::Selection,
    text_focus: gpui::FocusHandle,
    _updates: Task<()>,
}

impl EventEmitter<OpenSettings> for MatrixClient {}
impl EventEmitter<CacheChanged> for MatrixClient {}

impl MatrixClient {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (commands, mut updates) = backend::start();
        let listener = cx.spawn(async move |this, cx| {
            while let Some(update) = updates.next().await {
                if this.update(cx, |this, cx| this.apply(update, cx)).is_err() {
                    break;
                }
            }
        });
        // Wheel/scrollbar offsets need real heights for the entire loaded history,
        // including prepended rows outside the visible overdraw.
        let list = ListState::new(0, ListAlignment::Bottom, px(600.)).measure_all();
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event, _, cx| {
            if event.visible_range.start == 0 {
                let weak: WeakEntity<Self> = weak.clone();
                cx.defer(move |cx| {
                    weak.update(cx, |this, cx| this.older(cx)).ok();
                });
            }
        });
        let thread_list = ListState::new(0, ListAlignment::Bottom, px(400.));
        let owner = cx.entity_id();
        let retired_images = Rc::new(RefCell::new(Vec::new()));
        let retired = retired_images.clone();
        let client = Self {
            commands,
            phase: Phase::Starting,
            sign_in_error: None,
            choosing_server: false,
            signing_up: false,
            step: None,
            signup_busy: false,
            agreed: false,
            me: None,
            rooms: Vec::new(),
            space: None,
            space_trail: spaces::Trail::default(),
            space_transition: 0,
            space_backwards: false,
            linked_names: HashMap::new(),
            linked_name_requests: HashMap::new(),
            removing_space: None,
            room: None,
            rail_groups: rail_groups::Layout::default(),
            rail_hover: None,
            focused: true,
            in_tray: false,
            typing: typing::Remote::default(),
            typing_local: typing::Local::default(),
            typing_idle: None,
            typing_expiry: None,
            notifications: NotificationPreferences::default(),
            timelines: HashMap::new(),
            members: HashMap::new(),
            show_members: true,
            list: list.clone(),
            history_anchor: None,
            chat_scroll: cx.new(|_| {
                Scrollbar::list(list)
                    .named("chat-scrollbar")
                    .watching(owner)
            }),
            thread: None,
            thread_list: thread_list.clone(),
            thread_scroll: cx.new(|_| {
                Scrollbar::list(thread_list)
                    .named("thread-scrollbar")
                    .watching(owner)
            }),
            dialog: None,
            guard: Guard::Menu,
            security: None,
            banner_hidden: false,
            notice: None,
            notice_timer: None,
            refresh: None,
            next_local: 0,
            media: RefCell::new(
                Cache::new(32 * 1024 * 1024, 512, |media| match media {
                    Media::Ready(image) => image.bytes.len(),
                    _ => 0,
                })
                .on_remove(move |media| {
                    if let Media::Ready(image) = media {
                        retired.borrow_mut().push(image);
                    }
                }),
            ),
            retired_images,
            // Originals remain in the SDK's disk cache and can be read again.
            files: Cache::new(32 * 1024 * 1024, 64, |bytes| bytes.len()),
            cache_clearing: false,
            cache_status: None,
            viewing: None,
            media_zoom: media_zoom::Zoom::default(),
            download_jobs: Vec::new(),
            download_ticket: 0,
            media_menu: None,
            player_media: None,
            player: None,
            player_audio: None,
            audio_thread_timeline: ScrubberState::new("audio-thread-timeline"),
            player_image: None,
            painted_player_image: None,
            player_timeline: ScrubberState::new("media-timeline"),
            player_volume: ScrubberState::new("media-volume"),
            player_scrub: None,
            pending_play: None,
            person: None,
            line: None,
            replying: None,
            editing: None,
            picker: None,
            pending_focus: None,
            attachment: None,
            photo_compress: true,
            photo_size: PhotoSize::Balanced,
            compressing: None,
            attach_ticket: 0,
            profile_photo: None,
            profile_remove: false,
            profile_busy: false,
            server: cx.new(|cx| {
                let mut input = Input::new("huddle-server-hint", cx);
                input.set_text("matrix.org", cx);
                input
            }),
            username: cx.new(|cx| Input::new("huddle-username-hint", cx)),
            password: cx.new(|cx| Input::new("huddle-password-hint", cx).masked()),
            confirm_password: cx.new(|cx| Input::new("huddle-confirm-hint", cx).masked()),
            step_field: cx.new(|cx| Input::new("huddle-code-hint", cx)),
            draft: cx.new(|cx| Input::new("huddle-composer-hint", cx).tucked().growing()),
            thread_draft: cx.new(|cx| Input::new("huddle-thread-hint", cx).tucked().growing()),
            field: cx.new(|cx| Input::new("huddle-join-hint", cx)),
            profile_name: cx.new(|cx| Input::new("huddle-profile-hint", cx)),
            profile: profile::Panel::new(cx),
            public_profiles: Cache::new(512 * 1024, 128, profile::Public::bytes),
            person_loading: false,
            person_ticket: 0,
            shared_room_limit: 10,
            settings_room: None,
            room_name: cx.new(|cx| Input::new("Room name", cx)),
            room_topic: cx.new(|cx| Input::new("Room topic", cx).growing()),
            room_settings_busy: false,
            readers_view: None,
            own_read_targets: [None, None],
            admin: room_settings::Panel::new(cx),
            selection: text_selection::Selection::default(),
            text_focus: cx.focus_handle(),
            _updates: listener,
        };
        let command = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([
            gpui::KeyBinding::new(&format!("{command}-a"), ui::SelectAll, Some("MessageText")),
            gpui::KeyBinding::new("escape", Dismiss, Some("MessageText")),
        ]);
        let this = cx.weak_entity();
        App::on_action(cx, move |_: &Copy, cx: &mut App| {
            this.update(cx, |this, cx| this.copy_mark(cx)).ok();
        });
        cx.on_release(|this, cx| {
            this.media.get_mut().clear();
            this.release_retired_images(cx);
        })
        .detach();
        client
    }

    pub fn set_photo_send(&mut self, compress: bool, size: PhotoSize) {
        self.photo_compress = compress;
        self.photo_size = size;
    }

    fn command(&self, command: Command) {
        self.commands.send(command).ok();
    }

    fn my_id(&self) -> Option<OwnedUserId> {
        self.me.as_ref().map(|me| me.id.clone())
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        // A fetch that finished after hiding must not repopulate the media caches.
        if self.in_tray && matches!(&update, Update::Media { .. } | Update::VideoPreview { .. }) {
            return;
        }
        match update {
            Update::ProfileDetails {
                user,
                ticket,
                details,
            } => {
                if self.dialog != Some(Dialog::Profile)
                    || ticket != self.profile.ticket
                    || !self.me.as_ref().is_some_and(|me| me.id == user)
                {
                    return;
                }
                self.profile.loading = false;
                match details {
                    Ok(details) => {
                        let name = details.name.clone();
                        if let Some(me) = &mut self.me {
                            me.name.clone_from(&name);
                            me.avatar.clone_from(&details.avatar);
                        }
                        self.public_profiles
                            .insert(user.to_string(), details.public.clone());
                        self.profile_name
                            .update(cx, |input, cx| input.set_text(name, cx));
                        self.profile.loaded(details, cx);
                    }
                    Err(error) => self.profile.error = Some(error),
                }
            }
            Update::PublicProfile {
                user,
                viewer,
                ticket,
                details,
            } => {
                if !self.me.as_ref().is_some_and(|me| me.id == viewer)
                    || !self.person.as_ref().is_some_and(|person| person.id == user)
                    || ticket != self.person_ticket
                {
                    return;
                }
                self.person_loading = false;
                if let Some(person) = &mut self.person {
                    if let Some(room) = details
                        .room
                        .as_ref()
                        .filter(|room| Some(&room.id) == self.room.as_ref())
                    {
                        person.name.clone_from(&room.nickname);
                        person.avatar.clone_from(&room.avatar);
                    } else if !details.unavailable {
                        if let Some(name) = &details.name {
                            person.name.clone_from(name);
                        }
                        person.avatar.clone_from(&details.avatar);
                    }
                }
                self.public_profiles.insert(user.to_string(), details);
            }
            Update::Receipts { room, readers } => {
                if let Some(timeline) = self.timelines.get_mut(&room) {
                    let messages = &timeline.messages;
                    let threads = &timeline.threads;
                    let readers = readers
                        .into_iter()
                        .filter(|reader| match &reader.thread {
                            matrix_sdk::ruma::events::receipt::ReceiptThread::Thread(root) => {
                                threads.contains_key(root)
                            }
                            _ => true,
                        })
                        .collect();
                    let changed = timeline.receipts.merge(readers, |a, b, scope| {
                        receipts::compare(a, b, scope, messages, threads)
                    });
                    let places: Vec<_> = changed
                        .iter()
                        .filter_map(|event| timeline.locate(event))
                        .collect();
                    for place in places {
                        self.changed(&room, Change::Updated(place));
                    }
                }
            }
            Update::Typing { room, users } => {
                let names = users
                    .iter()
                    .map(|user| {
                        let name = self
                            .members
                            .get(&room)
                            .and_then(|members| members.iter().find(|member| &member.id == user))
                            .map(|member| member.name.as_str())
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| user.localpart());
                        one_line(name)
                    })
                    .collect();
                self.typing.update(room, names, std::time::Instant::now());
                if self.typing_expiry.is_none() && !self.typing.is_empty() {
                    self.typing_expiry = Some(cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor().timer(Duration::from_secs(1)).await;
                            let active = this
                                .update(cx, |this, cx| {
                                    this.typing.expire(std::time::Instant::now());
                                    cx.notify();
                                    if this.typing.is_empty() {
                                        this.typing_expiry = None;
                                        false
                                    } else {
                                        true
                                    }
                                })
                                .unwrap_or(false);
                            if !active {
                                break;
                            }
                        }
                    }));
                }
            }
            Update::RoomAdminLoaded { room, result } => {
                if self.settings_room.as_ref() != Some(&room) {
                    return;
                }
                self.admin.loading = false;
                match result {
                    Ok(snapshot) => {
                        self.admin.load(snapshot, cx);
                    }
                    Err(error) => {
                        self.admin.snapshot = None;
                        self.admin.status = Some(error);
                    }
                }
            }
            Update::RoomAdminDone { room, error } => {
                if self.settings_room.as_ref() != Some(&room) {
                    return;
                }
                self.room_settings_busy = false;
                self.admin.loading = true;
                self.admin.confirm = None;
                self.admin.status = Some(error.unwrap_or_else(|| "Settings updated.".into()));
            }
            Update::RoomSettingsSaved {
                room,
                name,
                topic,
                error,
            } => {
                if self.settings_room.as_ref() != Some(&room) {
                    return;
                }
                self.room_settings_busy = false;
                if let Some(error) = error {
                    self.notify(error, true, cx);
                } else {
                    if let Some(summary) = self.rooms.iter_mut().find(|known| known.id == room) {
                        if let Some(name) = name {
                            summary.explicit_name = (!name.is_empty()).then(|| name.clone());
                            if !name.is_empty() {
                                summary.name = name;
                            }
                        }
                        if let Some(topic) = topic {
                            summary.topic = (!topic.is_empty()).then_some(topic);
                        }
                    }
                    self.notify("Room settings saved.", false, cx);
                }
            }
            Update::SignedOut => {
                self.public_profiles.clear();
                self.profile.begin(cx);
                self.profile.loading = false;
                self.readers_view = None;
                self.stop_typing();
                self.typing = typing::Remote::default();
                self.typing_expiry = None;
                self.admin.reset();
                self.settings_room = None;
                self.room_settings_busy = false;
                self.cache_clearing = false;
                self.cache_status = None;
                cx.emit(CacheChanged);
                self.phase = Phase::SignedOut;
                for job in &self.download_jobs {
                    job.cancel.cancel();
                }
                self.download_jobs.clear();
                self.media_menu = None;
                self.me = None;
                self.rail_groups = rail_groups::Layout::default();
                self.rail_hover = None;
                self.rooms.clear();
                self.timelines.clear();
                self.members.clear();
                self.room = None;
                self.space = None;
                self.space_trail.clear();
                self.linked_names.clear();
                self.linked_name_requests.clear();
                self.removing_space = None;
                self.thread = None;
                self.dialog = None;
                self.security = None;
                self.replying = None;
                self.editing = None;
                self.picker = None;
                self.attachment = None;
                self.cancel_compress();
                self.selection.clear();
                self.viewing = None;
                self.player = None;
                self.player_audio = None;
                self.pending_play = None;
                self.player_image = None;
                self.player_scrub = None;
                self.files.clear();
                self.media.borrow_mut().clear();
                self.person = None;
                self.line = None;
                self.profile_photo = None;
                self.profile_remove = false;
                self.profile_busy = false;
                self.list.reset(0);
            }
            Update::SigningIn => {
                self.phase = Phase::SigningIn;
                self.sign_in_error = None;
            }
            Update::SignInFailed(problem) => {
                self.phase = Phase::SignedOut;
                self.sign_in_error = Some(problem.into());
            }
            Update::SignUp(step) => {
                self.signup_busy = false;
                self.sign_in_error = None;
                if matches!(step, Step::Token | Step::Email) {
                    let hint = match step {
                        Step::Token => "huddle-code-hint",
                        _ => "huddle-email-hint",
                    };
                    self.step_field.update(cx, |input, cx| {
                        input.set_text("", cx);
                        input.set_hint(hint, cx);
                    });
                    self.pending_focus = Some(Focus::Step);
                }
                self.agreed = false;
                self.step = Some(step);
            }
            Update::SignUpFailed(problem) => {
                self.signup_busy = false;
                self.sign_in_error = Some(problem.into());
            }
            Update::Profile(saved) => {
                if !self.me.as_ref().is_some_and(|me| me.id == saved.user) {
                    return;
                }
                let profile::Saved {
                    user: _,
                    name,
                    avatar,
                    public,
                    changed_name,
                    changed_avatar,
                    room: room_nickname,
                } = saved;
                self.profile_busy = false;
                self.dialog = None;
                if let Some(uri) = &avatar
                    && let Some(photo) = self.profile_photo.take()
                {
                    self.media.borrow_mut().insert(
                        format!("{uri}#avatar"),
                        Media::Ready(photo.preview.expect("profile image preview")),
                    );
                }
                self.profile_photo = None;
                self.profile_remove = false;
                let id = self.me.as_ref().map(|me| me.id.clone());
                if let Some(me) = &mut self.me {
                    me.name.clone_from(&name);
                    me.avatar.clone_from(&avatar);
                }
                if let Some(id) = id {
                    self.public_profiles.insert(id.to_string(), public);
                    for (room, members) in self.members.iter_mut() {
                        let nickname = room_nickname
                            .as_ref()
                            .filter(|(id, _)| id == room)
                            .map(|(_, name)| name);
                        for member in members.iter_mut() {
                            if member.id == id {
                                if let Some(nickname) = nickname {
                                    member.name.clone_from(nickname);
                                } else if changed_name {
                                    member.name.clone_from(&name);
                                }
                                if changed_avatar {
                                    member.avatar.clone_from(&avatar);
                                }
                            }
                        }
                    }
                    for (room, timeline) in self.timelines.iter_mut() {
                        let nickname = room_nickname
                            .as_ref()
                            .filter(|(id, _)| id == room)
                            .map(|(_, name)| name);
                        let rename = |message: &mut Message| {
                            if message.sender == id {
                                if let Some(nickname) = nickname {
                                    message.sender_name.clone_from(nickname);
                                } else if changed_name {
                                    message.sender_name.clone_from(&name);
                                }
                                if changed_avatar {
                                    message.avatar.clone_from(&avatar);
                                }
                            }
                        };
                        for message in &mut timeline.messages {
                            rename(message);
                        }
                        for replies in timeline.threads.values_mut() {
                            for message in replies {
                                rename(message);
                            }
                        }
                    }
                    if let Some(room) = &self.room
                        && let Some(timeline) = self.timelines.get(room)
                    {
                        let len = timeline.messages.len();
                        self.list.remeasure_items(0..len);
                    }
                }
                self.notify("Profile saved", false, cx);
            }
            Update::SignedIn { user, name, avatar } => {
                self.phase = Phase::SignedIn;
                self.rail_groups =
                    rail_groups::Layout::load(user.as_str()).unwrap_or_else(|error| {
                        log::warn!("Could not load navigation groups: {error:#}");
                        rail_groups::Layout::default()
                    });
                cx.emit(CacheChanged);
                self.me = Some(Me {
                    id: user,
                    name,
                    avatar,
                });
                self.signing_up = false;
                self.signup_busy = false;
                self.step = None;
                self.sign_in_error = None;
                for input in [&self.password, &self.confirm_password, &self.step_field] {
                    input.update(cx, |input, cx| input.set_text("", cx));
                }
            }
            Update::Download { ticket, progress } => {
                if let Some(job) = self
                    .download_jobs
                    .iter_mut()
                    .find(|job| job.ticket == ticket)
                {
                    job.progress = progress;
                }
            }
            Update::SpaceNames { user, names } => {
                if self.me.as_ref().is_some_and(|me| me.id == user) {
                    self.linked_names.extend(names);
                }
            }
            Update::Rooms(rooms) => {
                // A new child link should be discoverable without waiting for cache expiry.
                self.linked_name_requests.retain(|id, _| {
                    self.rooms
                        .iter()
                        .find(|room| &room.id == id)
                        .zip(rooms.iter().find(|room| &room.id == id))
                        .is_some_and(|(old, new)| {
                            old.children.len() == new.children.len()
                                && old
                                    .children
                                    .iter()
                                    .all(|child| new.children.contains(child))
                        })
                });
                self.rooms = rooms;
                if let Some(room) = &self.room
                    && !self
                        .rooms
                        .iter()
                        .any(|known| &known.id == room && !known.invite)
                {
                    self.stop_typing();
                    self.room = None;
                    self.thread = None;
                }
                if let Some(space) = &self.space
                    && !self.rooms.iter().any(|known| {
                        &known.id == space && known.kind == RoomKind::Space && !known.invite
                    })
                {
                    self.space = None;
                }
                self.space_trail.reconcile(&self.rooms, self.space.as_ref());
                self.load_linked_names();
            }
            Update::Unread(counts) => {
                for (id, unread) in counts {
                    if let Some(room) = self.rooms.iter_mut().find(|room| room.id == id) {
                        room.unread = unread;
                    }
                }
            }
            Update::History {
                room,
                events,
                token,
                older,
            } => self.history(room, events, token, older, cx),
            Update::Thread { room, root, events } => self.thread_loaded(room, root, events),
            Update::Members { room, members } => {
                self.members.insert(room, members);
            }
            Update::Live { room, event } => {
                let tag = room.to_string();
                if let Some((title, body)) = self.live(room, event) {
                    crate::presence::announce(
                        cx,
                        &title,
                        &body,
                        &tag,
                        self.notifications.attention,
                    );
                }
            }
            Update::Sent { room, local, event } => self.sent(room, local, event),
            Update::Opened(room) => {
                self.dialog = None;
                self.reveal(room.clone(), None, cx);
                if !self
                    .summary(&room)
                    .is_some_and(|room| room.kind == RoomKind::Space)
                {
                    self.pending_focus = Some(Focus::Draft);
                }
            }
            Update::CacheCleared { error } => {
                self.cache_clearing = false;
                self.cache_status = Some(error.unwrap_or_else(|| "Media cache cleared.".into()));
                cx.emit(CacheChanged);
            }
            Update::VideoPreview {
                key,
                bytes,
                thumbnail,
            } => {
                if let Some(bytes) = bytes {
                    self.files.insert(format!("{key}#original"), bytes.into());
                }
                self.media.borrow_mut().insert(
                    format!("{key}#poster"),
                    match thumbnail {
                        Some(bytes) => match sniff(&bytes) {
                            Some(format) => {
                                Media::Ready(Arc::new(Image::from_bytes(format, bytes)))
                            }
                            None => Media::Failed,
                        },
                        None => Media::Failed,
                    },
                );
            }
            Update::Media { key, bytes } => {
                if self
                    .pending_play
                    .as_ref()
                    .is_some_and(|media| key == media.key)
                {
                    let media = self.pending_play.take().unwrap();
                    if let Some(bytes) = bytes {
                        self.files.insert(
                            format!("{}#original", media.key),
                            Arc::from(bytes.as_slice()),
                        );
                        self.start_media(media, bytes, cx);
                    } else {
                        self.notify("This media file couldn't load. Try again.", true, cx);
                    }
                    return;
                }
                if key.ends_with("#original")
                    && let Some(bytes) = &bytes
                {
                    self.files.insert(key.clone(), Arc::from(bytes.as_slice()));
                }
                let image = bytes.and_then(|bytes| {
                    sniff(&bytes).map(|format| Arc::new(Image::from_bytes(format, bytes)))
                });
                self.media.borrow_mut().insert(
                    key,
                    match image {
                        Some(image) => Media::Ready(image),
                        None => Media::Failed,
                    },
                );
            }
            Update::Security { verified, recovery } => {
                self.security = Some((verified, recovery));
            }
            Update::Verify(Verify::Cancelled(_))
                if self.dialog != Some(Dialog::Security) || self.guard == Guard::Menu => {}
            Update::Verify(verify) => {
                self.guard = match verify {
                    Verify::Incoming => Guard::Incoming,
                    Verify::Waiting => Guard::Waiting,
                    Verify::Emojis(emojis) => Guard::Emojis(emojis),
                    Verify::Done => {
                        self.unlocked(cx);
                        Guard::Done
                    }
                    Verify::Cancelled(reason) => Guard::Failed(reason),
                };
                self.dialog = Some(Dialog::Security);
            }
            Update::RecoveryKey(key) => self.guard = Guard::NewKey(key),
            Update::Recovered => {
                self.guard = Guard::Done;
                self.field.update(cx, |input, cx| input.set_text("", cx));
                self.unlocked(cx);
            }
            Update::Failed(problem) => {
                self.profile_busy = false;
                if matches!(self.guard, Guard::Recovering) {
                    self.guard = Guard::Key;
                } else if matches!(self.guard, Guard::Waiting) {
                    self.guard = Guard::Menu;
                }
                self.notify(problem, true, cx);
            }
            Update::Ring { room, event } if !matches!(self.line, Some(Line::Live { .. })) => {
                let name = self
                    .summary(&room)
                    .map(|room| room.name.clone())
                    .unwrap_or_else(|| "Someone".into());
                if !self.focused && self.notifications.allows_call() {
                    crate::presence::announce(
                        cx,
                        "Incoming call",
                        &name,
                        room.as_str(),
                        self.notifications.attention,
                    );
                }
                self.line = Some(Line::Ringing { room, event, name });
            }
            Update::Ring { .. } => {}
            Update::Calling { room } => {
                let same =
                    matches!(&self.line, Some(Line::Live { room: live, .. }) if live == &room);
                if !same {
                    let name = self
                        .summary(&room)
                        .map(|room| room.name.clone())
                        .unwrap_or_else(|| "Call".into());
                    self.line = Some(Line::Live { room, name });
                }
            }
            Update::CallEnded => self.line = None,
        }
        // A live message can make a previously unknown receipt position visible.
        // Refresh the last sent message as well as the receipt's exact event row.
        if let Some(room) = self.room.clone() {
            let places: Vec<_> = self
                .own_read_targets
                .iter()
                .flatten()
                .filter_map(|id| self.timelines.get(&room)?.locate(id))
                .collect();
            for place in places {
                self.changed(&room, Change::Updated(place));
            }
        }
        self.release_retired_images(cx);
        cx.notify();
    }

    fn release_retired_images(&self, cx: &mut App) {
        // Evict the decoded GPUI asset too: dropping encoded bytes alone leaves
        // its full-resolution RGBA frames in the global asset cache indefinitely.
        for image in self.retired_images.borrow_mut().drain(..) {
            image.remove_asset(cx);
        }
    }

    /// Reloads timelines after keys arrive so older encrypted messages can be read.
    fn unlocked(&mut self, cx: &mut Context<Self>) {
        for timeline in self.timelines.values_mut() {
            timeline.loaded = false;
        }
        if let Some(room) = self.room.clone() {
            self.command(Command::Open {
                room: room.clone(),
                history: true,
            });
            self.refresh = Some(cx.spawn(async move |this, cx| {
                for wait in [4, 12] {
                    cx.background_executor()
                        .timer(Duration::from_secs(wait))
                        .await;
                    let reload = this.update(cx, |this, _| {
                        if this.room.as_ref() == Some(&room) {
                            this.command(Command::Open {
                                room: room.clone(),
                                history: true,
                            });
                        }
                    });
                    if reload.is_err() {
                        break;
                    }
                }
            }));
        }
    }

    fn history(
        &mut self,
        room: OwnedRoomId,
        events: Vec<Event>,
        token: Option<String>,
        older: bool,
        cx: &mut Context<Self>,
    ) {
        let current = self.room.as_ref() == Some(&room);
        let anchor = (older && current).then(|| {
            let offset = self.list.logical_scroll_top();
            let height = if offset.item_ix == 0 {
                self.list
                    .bounds_for_item(0)
                    .map(|bounds| bounds.size.height)
            } else {
                None
            };
            (offset, height)
        });
        let anchor_room = room.clone();
        let timeline = self.timelines.entry(room).or_default();
        let mut fresh = Vec::new();
        let mut relations = Vec::new();
        for event in events {
            match event {
                Event::Message(message) if message.thread.is_none() => fresh.push(message),
                Event::Message(_) => {}
                other => relations.push(other),
            }
        }
        timeline.token = token;
        timeline.loading = false;
        if older {
            let before = timeline.messages.len();
            timeline.messages.splice(0..0, fresh);
            for event in relations {
                if let Relate::Orphan(event) = timeline.relate(event) {
                    timeline.keep(event);
                }
            }
            timeline.retry();
            let added = timeline.messages.len().saturating_sub(before);
            if current {
                // Anchor at response time so scrolling while the page loads is respected.
                let (mut anchor, height) = anchor.expect("current history has an anchor");
                anchor.item_ix += added;
                self.list.splice(0..0, added);
                let len = timeline.messages.len();
                self.list
                    .remeasure_items(added.min(len)..(added + 1).min(len));
                // Restore after invalidation to rebase any pending layout adjustment too.
                self.list.scroll_to(anchor);
                if let Some(height) = height {
                    self.history_anchor = Some((anchor_room, anchor, height));
                }
                // A queued wheel animation still targets pixel offsets from before
                // the prepend. It must not overwrite the restored message anchor.
                self.chat_scroll.update(cx, |bar, _| bar.rebase());
            }
        } else {
            let pending = timeline
                .messages
                .drain(..)
                .filter(|message| message.id.is_none())
                .collect::<Vec<_>>();
            timeline.messages = fresh;
            timeline.messages.extend(pending);
            timeline.orphans.clear();
            timeline.loaded = true;
            for event in relations {
                if let Relate::Orphan(event) = timeline.relate(event) {
                    timeline.keep(event);
                }
            }
            if current {
                self.list.reset(timeline.messages.len());
            }
        }
    }

    fn thread_loaded(&mut self, room: OwnedRoomId, root: OwnedEventId, events: Vec<Event>) {
        let current = self.room.as_ref() == Some(&room);
        let timeline = self.timelines.entry(room).or_default();
        let pending = timeline
            .threads
            .remove(&root)
            .unwrap_or_default()
            .into_iter()
            .filter(|message| message.id.is_none());
        let mut replies = Vec::new();
        let mut relations = Vec::new();
        for event in events {
            match event {
                Event::Message(message) if message.thread.as_ref() == Some(&root) => {
                    replies.push(message)
                }
                Event::Message(_) => {}
                other => relations.push(other),
            }
        }
        replies.extend(pending);
        let count = replies.len();
        timeline.threads.insert(root.clone(), replies);
        for event in relations {
            if let Relate::Orphan(event) = timeline.relate(event) {
                timeline.keep(event);
            }
        }
        timeline.retry();
        if let Some(Place::Main(index)) = timeline.locate(&root) {
            let message = &mut timeline.messages[index];
            message.replies = message.replies.max(count as u64);
        }
        if current && self.thread.as_ref() == Some(&root) {
            self.thread_list.reset(count + 1);
        }
    }

    fn live(&mut self, room: OwnedRoomId, event: Event) -> Option<(String, String)> {
        let preview = match &event {
            Event::Message(message)
                if self.my_id().as_ref() != Some(&message.sender)
                    && self.notifications.allows_message(
                        self.summary(&room)
                            .is_some_and(|room| room.kind == RoomKind::Direct),
                        message.thread.is_some(),
                        message.mentioned,
                    ) =>
            {
                Some((self.alert(&room, message), message.thread.clone()))
            }
            _ => None,
        };
        let current = self.room.as_ref() == Some(&room);
        let me = self.my_id();
        let open_thread = self.thread.clone();
        let Some(timeline) = self.timelines.get_mut(&room).filter(|t| t.loaded) else {
            return preview.and_then(|(text, thread)| {
                self.wants_alert(&room, thread.as_ref()).then_some(text)
            });
        };
        let mut changes = Vec::new();
        let mut read = None;
        let mut fresh = false;
        match event {
            Event::Message(message) => {
                let mine = me.as_ref() == Some(&message.sender);
                let id = message.id.clone();
                if let Some(root) = message.thread.clone() {
                    let counted = match timeline.threads.get_mut(&root) {
                        Some(replies) => match insert(replies, message, mine) {
                            Insert::Appended(index) => {
                                if current && open_thread.as_ref() == Some(&root) {
                                    self.thread_list.splice(index + 1..index + 1, 1);
                                }
                                true
                            }
                            Insert::Replaced(index) => {
                                if current && open_thread.as_ref() == Some(&root) {
                                    self.thread_list.splice(index + 1..index + 2, 1);
                                }
                                false
                            }
                            Insert::Duplicate => false,
                        },
                        None => true,
                    };
                    fresh = counted;
                    if counted && let Some(Place::Main(index)) = timeline.locate(&root) {
                        timeline.messages[index].replies += 1;
                        changes.push(Change::Updated(Place::Main(index)));
                    }
                } else {
                    match insert(&mut timeline.messages, message, mine) {
                        Insert::Appended(index) => {
                            fresh = true;
                            if current {
                                self.list.splice(index..index, 1);
                                if !mine {
                                    read = id;
                                }
                            }
                        }
                        Insert::Replaced(index) => {
                            if current {
                                self.list.splice(index..index + 1, 1);
                            }
                        }
                        Insert::Duplicate => {}
                    }
                    timeline.retry();
                }
            }
            relation => match timeline.relate(relation) {
                Relate::Changed(change) => changes.push(change),
                Relate::Orphan(event) => timeline.keep(event),
                Relate::Nothing => {}
            },
        }
        for change in changes {
            self.changed(&room, change);
        }
        if let Some(event) = read {
            self.command(Command::Read {
                room: room.clone(),
                event,
            });
        }
        preview.and_then(|(text, thread)| {
            (fresh && self.wants_alert(&room, thread.as_ref())).then_some(text)
        })
    }

    pub fn cache_bytes(&self) -> usize {
        self.files.bytes() + self.media.borrow().bytes()
    }
    pub fn cache_status(&self) -> Option<&str> {
        self.cache_status.as_deref()
    }
    pub fn can_clear_cache(&self) -> bool {
        self.phase == Phase::SignedIn && !self.cache_clearing
    }
    pub fn clear_cache(&mut self, cx: &mut Context<Self>) {
        if !self.can_clear_cache() {
            return;
        }
        self.files.retain(|key| key.starts_with("local:"));
        self.media
            .borrow_mut()
            .retain(|key| key.starts_with("local:"));
        self.release_retired_images(cx);
        self.cache_clearing = true;
        self.cache_status = Some("Clearing media cache…".into());
        self.command(Command::ClearCache);
        cx.emit(CacheChanged);
        cx.notify();
    }

    pub fn set_notifications(&mut self, preferences: NotificationPreferences) {
        self.notifications = preferences;
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused && !self.in_tray;
        if !self.focused {
            self.stop_typing();
        }
    }

    pub fn set_in_tray(&mut self, hidden: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.in_tray = hidden;
        if hidden {
            self.stop_typing();
            self.focused = false;
            self.pending_play = None;
            self.player = None;
            self.player_audio = None;
            self.player_image = None;
            self.release_player_texture(window);
            self.player_scrub = None;
            self.viewing = None;
            // Keep unsent local attachments; remote media reloads from the disk cache.
            self.files.retain(|key| key.starts_with("local:"));
            self.media
                .borrow_mut()
                .retain(|key| key.starts_with("local:"));
            self.release_retired_images(cx);
        }
        cx.emit(CacheChanged);
        cx.notify();
    }

    fn release_player_texture(&mut self, window: &mut Window) {
        if self.painted_player_image.as_ref().map(|image| image.id)
            != self.player_image.as_ref().map(|image| image.id)
        {
            if let Some(previous) = self.painted_player_image.take() {
                window.drop_image(previous).ok();
            }
            self.painted_player_image = self.player_image.clone();
        }
    }

    pub fn dismiss_alert(&self, cx: &mut Context<Self>) {
        if let Some(room) = &self.room {
            cx.dismiss_system_notification(room.as_str());
        }
    }

    pub fn focus_room(&mut self, room: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(room) = OwnedRoomId::try_from(room) else {
            return;
        };
        self.open(room, window, cx);
    }

    fn wants_alert(&self, room: &OwnedRoomId, thread: Option<&OwnedEventId>) -> bool {
        !(self.focused && self.room.as_ref() == Some(room) && self.thread.as_ref() == thread)
    }

    fn alert(&self, room: &OwnedRoomId, message: &Message) -> (String, String) {
        let body = match message.kind {
            MessageKind::Image => "Sent an image".to_owned(),
            MessageKind::Attachment if message.body.is_empty() => "Sent a file".to_owned(),
            MessageKind::Encrypted => "New message".to_owned(),
            MessageKind::Emote => format!("* {}", one_line(&message.body)),
            _ => one_line(&message.body),
        };
        let body = self.notifications.preview(body);
        let direct = self
            .summary(room)
            .is_some_and(|room| room.kind == RoomKind::Direct);
        if direct {
            return (message.sender_name.clone(), body);
        }
        let title = self
            .summary(room)
            .map(|room| room.name.clone())
            .unwrap_or_else(|| "Huddle".to_owned());
        let body = match message.sender_name.is_empty() {
            true => body,
            false => format!("{}: {body}", message.sender_name),
        };
        (title, body)
    }

    fn changed(&mut self, room: &OwnedRoomId, change: Change) {
        if self.room.as_ref() != Some(room) {
            return;
        }
        let Some(timeline) = self.timelines.get(room) else {
            return;
        };
        match change {
            Change::Updated(Place::Main(index)) => {
                self.list.remeasure_items(index..index + 1);
                if self.thread.is_some()
                    && timeline.messages.get(index).and_then(|m| m.id.as_ref())
                        == self.thread.as_ref()
                {
                    self.thread_list.remeasure_items(0..1);
                }
            }
            Change::Updated(Place::Thread(root, index)) => {
                if self.thread.as_ref() == Some(&root) {
                    self.thread_list.remeasure_items(index + 1..index + 2);
                }
            }
            Change::Removed(Place::Main(index), id) => {
                self.list.splice(index..index + 1, 0);
                if index < timeline.messages.len() {
                    self.list.remeasure_items(index..index + 1);
                }
                if self.thread.as_ref() == Some(&id) {
                    self.thread = None;
                }
            }
            Change::Removed(Place::Thread(root, index), _) => {
                if self.thread.as_ref() == Some(&root) {
                    self.thread_list.splice(index + 1..index + 2, 0);
                }
            }
        }
    }

    fn sent(&mut self, room: OwnedRoomId, local: u64, event: Option<OwnedEventId>) {
        let current = self.room.as_ref() == Some(&room);
        let open_thread = self.thread.clone();
        let Some(timeline) = self.timelines.get_mut(&room) else {
            return;
        };
        let is_pending = |message: &Message| message.id.is_none() && message.local == Some(local);
        let place = match timeline.messages.iter().position(is_pending) {
            Some(index) => Place::Main(index),
            None => match timeline.threads.iter().find_map(|(root, replies)| {
                replies
                    .iter()
                    .position(is_pending)
                    .map(|index| Place::Thread(root.clone(), index))
            }) {
                Some(place) => place,
                None => return,
            },
        };
        let echoed = event
            .as_ref()
            .is_some_and(|event| timeline.locate(event).is_some());
        let (messages, list, offset, visible) = match &place {
            Place::Main(_) => (&mut timeline.messages, &self.list, 0, current),
            Place::Thread(root, _) => (
                timeline.threads.get_mut(root).expect("located above"),
                &self.thread_list,
                1,
                current && open_thread.as_ref() == Some(root),
            ),
        };
        let index = match place {
            Place::Main(index) | Place::Thread(_, index) => index,
        };
        match event {
            Some(event) if !echoed => {
                messages[index].id = Some(event);
                if visible {
                    list.splice(index + offset..index + offset + 1, 1);
                }
            }
            _ => {
                messages.remove(index);
                if visible {
                    list.splice(index + offset..index + offset + 1, 0);
                }
            }
        }
    }

    fn older(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.room.clone() else {
            return;
        };
        let Some(timeline) = self.timelines.get_mut(&room) else {
            return;
        };
        if !timeline.loaded || timeline.loading {
            return;
        }
        let Some(from) = timeline.token.clone() else {
            return;
        };
        timeline.loading = true;
        self.command(Command::Older { room, from });
        cx.notify();
    }

    fn notify(&mut self, text: impl Into<SharedString>, failed: bool, cx: &mut Context<Self>) {
        self.notice = Some((text.into(), failed));
        self.notice_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE).await;
            this.update(cx, |this, cx| {
                this.notice = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn summary(&self, id: &OwnedRoomId) -> Option<&RoomSummary> {
        self.rooms.iter().find(|room| &room.id == id)
    }

    fn space_of(&self, id: &OwnedRoomId) -> Option<&RoomSummary> {
        self.rooms.iter().find(|room| {
            room.kind == RoomKind::Space
                && !room.invite
                && &room.id != id
                && room.children.contains(id)
        })
    }

    /// A room shown on the far-left rail: joined, not a direct message, and not inside a space.
    fn loose(&self, id: &OwnedRoomId) -> bool {
        let listed = self
            .summary(id)
            .is_some_and(|room| room.kind == RoomKind::Room && !room.invite);
        listed && self.space_of(id).is_none()
    }

    /// Home is the direct-message list. A room picked from the rail leaves that list.
    fn on_home(&self) -> bool {
        self.space.is_none() && !self.room.as_ref().is_some_and(|id| self.loose(id))
    }

    fn timeline(&self) -> Option<&Timeline> {
        self.room.as_ref().and_then(|room| self.timelines.get(room))
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase == Phase::SigningIn {
            return;
        }
        let username = self.username.read(cx).text().trim().to_owned();
        let password = self.password.read(cx).text().to_owned();
        if username.is_empty() {
            self.sign_in_error = Some("Enter your username to continue.".into());
            focus(&self.username, window, cx);
        } else if password.is_empty() {
            self.sign_in_error = Some("Enter your password to continue.".into());
            focus(&self.password, window, cx);
        } else {
            let server = self.server.read(cx).text().to_owned();
            self.command(Command::SignIn {
                server,
                username,
                password,
            });
        }
        cx.notify();
    }

    fn sign_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signup_busy {
            return;
        }
        let username = self.username.read(cx).text().trim().to_owned();
        let password = self.password.read(cx).text().to_owned();
        let again = self.confirm_password.read(cx).text().to_owned();
        let problem = if username.is_empty() {
            focus(&self.username, window, cx);
            Some("Pick a username to continue.")
        } else if password.chars().count() < 8 {
            focus(&self.password, window, cx);
            Some("Use a password with at least 8 characters.")
        } else if password != again {
            focus(&self.confirm_password, window, cx);
            Some("The passwords don't match.")
        } else {
            None
        };
        self.sign_in_error = problem.map(Into::into);
        if problem.is_none() {
            self.signup_busy = true;
            let server = self.server.read(cx).text().to_owned();
            self.command(Command::SignUp {
                server,
                username,
                password,
            });
        }
        cx.notify();
    }

    fn answer(&mut self, answer: Answer, cx: &mut Context<Self>) {
        self.signup_busy = true;
        self.sign_in_error = None;
        self.command(Command::Answer(answer));
        cx.notify();
    }

    fn submit_step(&mut self, cx: &mut Context<Self>) {
        let text = self.step_field.read(cx).text().trim().to_owned();
        match self.step {
            Some(Step::Token) if !text.is_empty() => self.answer(Answer::Token(text), cx),
            Some(Step::Email) if text.contains('@') => self.answer(Answer::Email(text), cx),
            Some(Step::Email) => {
                self.sign_in_error = Some("Enter a valid email address.".into());
                cx.notify();
            }
            _ => {}
        }
    }

    fn leave_sign_up(&mut self, cx: &mut Context<Self>) {
        self.command(Command::CancelSignUp);
        self.step = None;
        self.signup_busy = false;
        self.sign_in_error = None;
        cx.notify();
    }

    fn open(&mut self, id: OwnedRoomId, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal(id, Some(window), cx);
    }

    fn reveal(&mut self, id: OwnedRoomId, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self
            .summary(&id)
            .is_some_and(|room| room.kind == RoomKind::Space)
        {
            self.enter_space(id, true, cx);
            return;
        }
        let direct = self
            .summary(&id)
            .is_some_and(|room| room.kind == RoomKind::Direct);
        let in_view = match &self.space {
            Some(space) => self
                .summary(space)
                .is_some_and(|space| space.children.contains(&id)),
            None => direct || self.space_of(&id).is_none(),
        };
        if !in_view {
            self.space_trail.clear();
            self.space = (!direct)
                .then(|| self.space_of(&id).map(|space| space.id.clone()))
                .flatten();
        }
        if self.room.as_ref() != Some(&id) {
            self.readers_view = None;
            self.stop_typing();
            self.cancel_compress();
            self.thread = None;
            self.replying = None;
            self.editing = None;
            self.picker = None;
        }
        self.room = Some(id.clone());
        let timeline = self.timelines.get(&id);
        let loaded = timeline.is_some_and(|timeline| timeline.loaded);
        let count = timeline.map_or(0, |timeline| timeline.messages.len());
        let latest =
            timeline.and_then(|timeline| timeline.messages.iter().rev().find_map(|m| m.id.clone()));
        self.list.reset(count);
        self.command(Command::Open {
            room: id.clone(),
            history: !loaded,
        });
        if loaded && let Some(event) = latest {
            self.command(Command::Read { room: id, event });
        }
        if let Some(window) = window {
            focus(&self.draft, window, cx);
        }
        cx.notify();
    }

    fn open_thread(&mut self, root: OwnedEventId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.room.clone() else {
            return;
        };
        let count = self
            .timelines
            .get(&room)
            .and_then(|timeline| timeline.threads.get(&root))
            .map_or(0, Vec::len);
        self.thread = Some(root.clone());
        self.picker = None;
        if matches!(self.editing, Some((_, Spot::Thread))) {
            self.editing = None;
        }
        self.thread_list.reset(count + 1);
        self.command(Command::Thread { room, root });
        focus(&self.thread_draft, window, cx);
        cx.notify();
    }

    fn send(&mut self, spot: Spot, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_typing();
        let (Some(room), Some(me)) = (self.room.clone(), self.me.as_ref()) else {
            return;
        };
        let input = match spot {
            Spot::Main => self.draft.clone(),
            Spot::Thread => self.thread_draft.clone(),
        };
        let body = input.read(cx).text().trim().to_owned();
        if self.compressing == Some(spot) {
            self.notify("Still preparing that file.", false, cx);
            return;
        }
        let shot = self
            .attachment
            .as_ref()
            .filter(|(at, _)| *at == spot)
            .map(|(_, shot)| shot.name.clone());
        if body.is_empty() && shot.is_none() {
            return;
        }

        if let Some((event, at)) = self.editing.clone()
            && at == spot
        {
            self.editing = None;
            let me = me.id.clone();
            let rich = rich::markdown(&body);
            self.live(
                room.clone(),
                Event::Edit {
                    target: event.clone(),
                    sender: me,
                    body: body.clone(),
                    rich,
                },
            );
            self.command(Command::Edit { room, event, body });
            input.update(cx, |input, cx| input.set_text("", cx));
            focus(&input, window, cx);
            cx.notify();
            return;
        }

        if shot.is_some() {
            self.send_picture(spot, body, window, cx);
            return;
        }

        let local = self.next_local;
        self.next_local += 1;
        let reply = match spot {
            Spot::Main => self.replying.take(),
            Spot::Thread => None,
        };
        let thread = match spot {
            Spot::Main => None,
            Spot::Thread => self.thread.clone(),
        };
        let message = Message {
            id: None,
            local: Some(local),
            sender: me.id.clone(),
            sender_name: me.name.clone(),
            avatar: me.avatar.clone(),
            body: body.clone(),
            rich: rich::markdown(&body),
            kind: MessageKind::Text,
            picture: None,
            timestamp: Local::now().timestamp_millis(),
            edited: false,
            reply: reply.as_ref().map(|target| target.id.clone()),
            thread: thread.clone(),
            replies: 0,
            reactions: Vec::new(),
            mentioned: false,
        };
        let timeline = self.timelines.entry(room.clone()).or_default();
        let thread = match thread {
            Some(root) => {
                let replies = timeline.threads.entry(root.clone()).or_default();
                let latest = replies
                    .iter()
                    .rev()
                    .find_map(|message| message.id.clone())
                    .unwrap_or_else(|| root.clone());
                let index = replies.len();
                replies.push(message);
                self.thread_list.splice(index + 1..index + 1, 1);
                if let Some(Place::Main(index)) = timeline.locate(&root) {
                    timeline.messages[index].replies += 1;
                    self.list.remeasure_items(index..index + 1);
                }
                Some((root, latest))
            }
            None => {
                let index = timeline.messages.len();
                timeline.messages.push(message);
                self.list.splice(index..index, 1);
                None
            }
        };
        input.update(cx, |input, cx| input.set_text("", cx));
        focus(&input, window, cx);
        self.command(Command::Send {
            room,
            body,
            local,
            reply: reply.map(|target| (target.id, target.sender)),
            thread,
        });
        cx.notify();
    }

    fn send_picture(
        &mut self,
        spot: Spot,
        caption: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(room), Some(me), Some((_, shot))) = (
            self.room.clone(),
            self.me.as_ref(),
            self.attachment.take().filter(|(at, _)| *at == spot),
        ) else {
            return;
        };
        let local = self.next_local;
        self.next_local += 1;
        let reply = match spot {
            Spot::Main => self.replying.take(),
            Spot::Thread => None,
        };
        let thread = match spot {
            Spot::Main => None,
            Spot::Thread => self.thread.clone(),
        };
        let key = format!("local:{local}#image");
        if let Some(preview) = &shot.preview {
            self.media.borrow_mut().insert(
                if shot.mime.starts_with("video/") {
                    format!("{key}#poster")
                } else {
                    key.clone()
                },
                Media::Ready(preview.clone()),
            );
        }
        self.files
            .insert(format!("{key}#original"), Arc::from(shot.bytes.as_slice()));
        let body = match caption.is_empty() {
            true => shot.name.clone(),
            false => caption.clone(),
        };
        let message = Message {
            id: None,
            local: Some(local),
            sender: me.id.clone(),
            sender_name: me.name.clone(),
            avatar: me.avatar.clone(),
            body: body.clone(),
            rich: match caption.is_empty() {
                true => rich::Rich::new(),
                false => rich::markdown(&caption),
            },
            kind: if shot.mime.starts_with("image/") {
                MessageKind::Image
            } else {
                MessageKind::Attachment
            },
            picture: Some(Picture {
                size: Some(shot.bytes.len() as u64),
                mime: (!shot.mime.starts_with("image/")).then(|| shot.mime.clone()),
                thumbnail: None,
                duration: shot.duration,
                key,
                source: MediaSource::Plain(
                    <&matrix_sdk::ruma::MxcUri>::from("mxc://huddle/local").to_owned(),
                ),
                width: shot.width,
                height: shot.height,
                name: shot.name.clone(),
            }),
            timestamp: Local::now().timestamp_millis(),
            edited: false,
            reply: reply.as_ref().map(|target| target.id.clone()),
            thread: thread.clone(),
            replies: 0,
            reactions: Vec::new(),
            mentioned: false,
        };
        let timeline = self.timelines.entry(room.clone()).or_default();
        let thread = match thread {
            Some(root) => {
                let replies = timeline.threads.entry(root.clone()).or_default();
                let latest = replies
                    .iter()
                    .rev()
                    .find_map(|message| message.id.clone())
                    .unwrap_or_else(|| root.clone());
                let index = replies.len();
                replies.push(message);
                self.thread_list.splice(index + 1..index + 1, 1);
                if let Some(Place::Main(index)) = timeline.locate(&root) {
                    timeline.messages[index].replies += 1;
                    self.list.remeasure_items(index..index + 1);
                }
                Some((root, latest))
            }
            None => {
                let index = timeline.messages.len();
                timeline.messages.push(message);
                self.list.splice(index..index, 1);
                None
            }
        };
        let input = match spot {
            Spot::Main => &self.draft,
            Spot::Thread => &self.thread_draft,
        };
        input.update(cx, |input, cx| input.set_text("", cx));
        focus(input, window, cx);
        self.command(Command::Picture {
            room,
            name: shot.name,
            mime: shot.mime,
            bytes: shot.bytes,
            width: shot.width,
            height: shot.height,
            caption,
            duration: shot.duration,
            thumbnail: shot.thumbnail,
            local,
            reply: reply.map(|target| (target.id, target.sender)),
            thread,
        });
        cx.notify();
    }

    fn pick_image(&mut self, spot: Spot, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an image, video or audio file".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.attach_file(spot, path, cx))
                .ok();
        })
        .detach();
    }

    fn drop_files(
        &mut self,
        spot: Spot,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let [path] = paths.paths() else {
            self.notify("Drop one file at a time.", true, cx);
            return;
        };
        self.attach_file(spot, path.clone(), cx);
        let input = match spot {
            Spot::Main => &self.draft,
            Spot::Thread => &self.thread_draft,
        };
        focus(input, window, cx);
    }

    fn attach_file(&mut self, spot: Spot, path: std::path::PathBuf, cx: &mut Context<Self>) {
        if matches!(&self.editing, Some((_, at)) if *at == spot) {
            self.notify("Finish editing before adding a file.", true, cx);
            return;
        }
        let Some(room) = self.room.clone() else {
            return;
        };
        if self.summary(&room).is_none_or(|room| room.invite) {
            return;
        }
        let thread = (spot == Spot::Thread)
            .then(|| self.thread.clone())
            .flatten();
        if spot == Spot::Thread && thread.is_none() {
            return;
        }
        self.cancel_compress();
        let ticket = self.attach_ticket;
        self.compressing = Some(spot);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { upload::read(&path) })
                .await;
            this.update(cx, |this, cx| {
                if this.attach_ticket != ticket {
                    return;
                }
                this.compressing = None;
                if this.room.as_ref() != Some(&room)
                    || (spot == Spot::Thread && this.thread != thread)
                    || matches!(&this.editing, Some((_, at)) if *at == spot)
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok((name, bytes)) => this.attach(spot, name, Some(bytes), cx),
                    Err(error) => this.notify(error, true, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_photo(&mut self, cx: &mut Context<Self>) {
        if self.profile_busy || self.profile.loading {
            return;
        }
        let ticket = self.profile.ticket;
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a photo".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_spawn(async move { upload::read(&path) })
                .await;
            this.update(cx, |this, cx| {
                if this.dialog != Some(Dialog::Profile)
                    || this.profile.ticket != ticket
                    || this.profile_busy
                {
                    return;
                }
                let (name, bytes) = match result {
                    Ok(file) => file,
                    Err(error) => {
                        this.notify(error, true, cx);
                        return;
                    }
                };
                if let Some(photo) = this.read_image(&name, Some(bytes), cx) {
                    this.profile_photo = Some(photo);
                    this.profile_remove = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn attach(&mut self, spot: Spot, name: String, bytes: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(bytes) = bytes else {
            self.notify("That file couldn't be opened.", true, cx);
            return;
        };
        if bytes.len() > UPLOAD_LIMIT {
            self.notify("That file is too large. Choose one under 20 MB.", true, cx);
            return;
        }
        self.cancel_compress();
        if sniff(&bytes).is_none() {
            if let Some(mime) = playback::mime(&name) {
                self.cancel_compress();
                self.attachment = None;
                self.compressing = Some(spot);
                let ticket = self.attach_ticket;
                let room = self.room.clone();
                let thread = self.thread.clone();
                cx.spawn(async move |this, cx| {
                    let (bytes, result) = cx
                        .background_spawn(async move {
                            let result = playback::inspect(&bytes, mime);
                            (bytes, result)
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        if ticket != this.attach_ticket {
                            return;
                        }
                        this.compressing = None;
                        if this.room != room || (spot == Spot::Thread && this.thread != thread) {
                            cx.notify();
                            return;
                        }
                        match result {
                            Ok(info) => {
                                this.attachment = Some((
                                    spot,
                                    Upload {
                                        name,
                                        bytes,
                                        mime: mime.into(),
                                        width: info.width,
                                        height: info.height,
                                        duration: info.duration,
                                        preview: info.thumbnail.as_ref().map(|p| {
                                            Arc::new(Image::from_bytes(
                                                ImageFormat::Jpeg,
                                                p.bytes.clone(),
                                            ))
                                        }),
                                        thumbnail: info.thumbnail,
                                    },
                                ));
                                cx.notify();
                            }
                            Err(error) => {
                                log::warn!("Media validation failed: {error:#}");
                                this.notify(
                                    "This file is damaged or uses an unsupported codec.",
                                    true,
                                    cx,
                                );
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            } else {
                self.notify("Choose a supported image, video or audio file.", true, cx);
            }
            return;
        }
        if !self.photo_compress || !photo::can_compress(&bytes) {
            self.place_upload(spot, name, bytes, cx);
            return;
        }
        let longest = self.photo_size.longest();
        self.attach_ticket = self.attach_ticket.wrapping_add(1);
        let ticket = self.attach_ticket;
        if self.attachment.as_ref().is_some_and(|(at, _)| *at == spot) {
            self.attachment = None;
        }
        self.compressing = Some(spot);
        let room = self.room.clone();
        let thread = self.thread.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_spawn(async move { photo::compress(name, bytes, longest) })
                .await;
            this.update(cx, |this, cx| {
                if this.attach_ticket != ticket {
                    return;
                }
                this.compressing = None;
                if this.room != room || (spot == Spot::Thread && this.thread != thread) {
                    cx.notify();
                    return;
                }
                this.place_upload(spot, prepared.name, prepared.bytes, cx);
            })
            .ok();
        })
        .detach();
    }

    fn place_upload(&mut self, spot: Spot, name: String, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(shot) = self.read_image(&name, Some(bytes), cx) {
            self.attachment = Some((spot, shot));
            cx.notify();
        }
    }

    fn cancel_compress(&mut self) {
        self.compressing = None;
        self.attach_ticket = self.attach_ticket.wrapping_add(1);
    }

    fn read_image(
        &mut self,
        name: &str,
        bytes: Option<Vec<u8>>,
        cx: &mut Context<Self>,
    ) -> Option<Upload> {
        let Some(bytes) = bytes else {
            self.notify("That file couldn't be opened.", true, cx);
            return None;
        };
        if bytes.len() > UPLOAD_LIMIT {
            self.notify("That file is too large. Choose one under 20 MB.", true, cx);
            return None;
        }
        let Some(format) = sniff(&bytes) else {
            self.notify("Choose a PNG, JPEG, GIF or WebP image.", true, cx);
            return None;
        };
        let (width, height) = measure(&bytes);
        Some(Upload {
            name: name.to_owned(),
            mime: mime_of(format).to_owned(),
            preview: Some(Arc::new(Image::from_bytes(format, bytes.clone()))),
            duration: None,
            thumbnail: None,
            bytes,
            width,
            height,
        })
    }

    fn start_reply(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = message.id.clone() else { return };
        self.editing = None;
        self.replying = Some(Target {
            id,
            sender: message.sender.clone(),
            name: message.sender_name.clone(),
        });
        focus(&self.draft, window, cx);
        cx.notify();
    }

    fn start_edit(
        &mut self,
        message: &Message,
        spot: Spot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = message.id.clone() else { return };
        self.replying = None;
        self.attachment = None;
        self.cancel_compress();
        self.editing = Some((id, spot));
        let input = match spot {
            Spot::Main => &self.draft,
            Spot::Thread => &self.thread_draft,
        };
        input.update(cx, |input, cx| input.set_text(message.body.clone(), cx));
        focus(input, window, cx);
        cx.notify();
    }

    fn cancel_compose(&mut self, spot: Spot, cx: &mut Context<Self>) {
        let editing = matches!(&self.editing, Some((_, at)) if *at == spot);
        if editing {
            self.editing = None;
            let input = match spot {
                Spot::Main => &self.draft,
                Spot::Thread => &self.thread_draft,
            };
            input.update(cx, |input, cx| input.set_text("", cx));
        } else if spot == Spot::Main {
            self.replying = None;
        }
        self.picker = None;
        cx.notify();
    }

    fn react(&mut self, message: &Message, key: &str, cx: &mut Context<Self>) {
        let (Some(room), Some(event), Some(me)) =
            (self.room.clone(), message.id.clone(), self.my_id())
        else {
            return;
        };
        self.picker = None;
        let mine = message
            .reactions
            .iter()
            .find(|reaction| reaction.key == key)
            .and_then(|reaction| reaction.senders.iter().find(|(sender, _)| sender == &me))
            .map(|(_, id)| id.clone());
        match mine {
            Some(reaction) => {
                self.live(room.clone(), Event::Redact(reaction.clone()));
                self.command(Command::Redact {
                    room,
                    event: reaction,
                });
            }
            None => self.command(Command::React {
                room,
                event,
                key: key.to_owned(),
            }),
        }
        cx.notify();
    }

    fn delete(&mut self, message: &Message, cx: &mut Context<Self>) {
        let (Some(room), Some(event)) = (self.room.clone(), message.id.clone()) else {
            return;
        };
        self.live(room.clone(), Event::Redact(event.clone()));
        self.command(Command::Redact { room, event });
        cx.notify();
    }

    fn follow(&mut self, target: &str, cx: &mut Context<Self>) {
        let address = target
            .strip_prefix("https://matrix.to/#/")
            .map(|rest| rest.split('?').next().unwrap_or(rest));
        match address {
            Some(user) if user.starts_with('@') => {
                self.command(Command::Talk(user.to_owned()));
                self.notify("Opening the conversation…", false, cx);
            }
            Some(room) if room.starts_with('#') || room.starts_with('!') => {
                let room = room.split('/').next().unwrap_or(room);
                self.command(Command::Join(room.to_owned()));
                self.notify("Opening the room…", false, cx);
            }
            _ => cx.open_url(target),
        }
    }

    fn on_select(&self, cx: &Context<Self>) -> rich::OnSelect {
        let this = cx.weak_entity();
        Rc::new(move |window, cx| {
            this.update(cx, |this, cx| {
                this.text_focus.focus(window, cx);
                this.release_drafts(cx);
                cx.notify();
            })
            .ok();
        })
    }

    fn prepare_selection(&self) {
        let mut document = Vec::new();
        if let Some(timeline) = self.timeline() {
            for message in &timeline.messages {
                rich::document(
                    &format!("m-{}", message_key(message)),
                    &message.rich,
                    &mut document,
                );
            }
            if let Some(root) = &self.thread {
                if let Some(message) = timeline.find(root) {
                    rich::document(
                        &format!("t-{}", message_key(message)),
                        &message.rich,
                        &mut document,
                    );
                }
                if let Some(messages) = timeline.threads.get(root) {
                    for message in messages {
                        rich::document(
                            &format!("t-{}", message_key(message)),
                            &message.rich,
                            &mut document,
                        );
                    }
                }
            }
        }
        self.selection.prepare(
            self.room
                .as_ref()
                .map_or(String::new(), ToString::to_string),
            document,
        );
    }

    fn extend_selection(&self, mut position: gpui::Point<Pixels>) {
        let list = if self.selection.thread() {
            &self.thread_list
        } else {
            &self.list
        };
        let bounds = list.viewport_bounds();
        if bounds.size.height > px(1.) {
            position.y = position.y.max(bounds.top()).min(bounds.bottom() - px(0.5));
        }
        self.selection.extend(position);
    }

    fn release_drafts(&mut self, cx: &mut Context<Self>) {
        for draft in [&self.draft, &self.thread_draft] {
            draft.update(cx, |input, cx| {
                if input.has_selection() {
                    input.collapse(cx);
                }
            });
        }
    }

    fn copy_mark(&mut self, cx: &mut Context<Self>) {
        let text = self.selection.text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn stop_typing(&mut self) {
        self.typing_idle = None;
        if let Some(room) = self.typing_local.stop() {
            self.command(Command::Typing {
                room,
                active: false,
            });
        }
    }

    fn composer_typed(&mut self, spot: Spot, text: &str, cx: &mut Context<Self>) {
        if !self.focused || self.in_tray || text.trim().is_empty() {
            self.stop_typing();
            return;
        }
        let Some(room) = self
            .room
            .clone()
            .filter(|id| self.summary(id).is_some_and(|room| !room.invite))
        else {
            return;
        };
        if spot == Spot::Thread && self.thread.is_none() {
            return;
        }
        if self.typing_local.activity(&room, std::time::Instant::now()) {
            self.command(Command::Typing { room, active: true });
        }
        self.typing_idle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(typing::IDLE).await;
            this.update(cx, |this, _| this.stop_typing()).ok();
        }));
    }

    fn wire_composer(&mut self, cx: &mut Context<Self>) {
        if self.draft.read(cx).pastes_images() {
            return;
        }
        let this = cx.entity().downgrade();
        for (input, spot) in [
            (self.draft.clone(), Spot::Main),
            (self.thread_draft.clone(), Spot::Thread),
        ] {
            let pasted = this.clone();
            let active = this.clone();
            let edited = this.clone();
            input.update(cx, |input, _| {
                input.on_edit(move |text, cx| {
                    edited
                        .update(cx, |this, cx| this.composer_typed(spot, text, cx))
                        .ok();
                });
                input.on_paste_image(move |picture, _, cx| {
                    pasted
                        .update(cx, |this, cx| this.paste_picture(spot, picture, cx))
                        .ok();
                });
                input.on_activity(move |_, cx| {
                    active
                        .update(cx, |this, cx| {
                            if this.selection.clear() {
                                cx.notify();
                            }
                        })
                        .ok();
                });
            });
        }
    }

    fn paste_picture(&mut self, spot: Spot, picture: PastedPicture, cx: &mut Context<Self>) {
        if matches!(&self.editing, Some((_, at)) if *at == spot) {
            self.notify("Finish editing before adding an image.", true, cx);
            return;
        }
        match picture {
            PastedPicture::Image(image) => {
                let name = format!("Pasted image.{}", image.format().extension());
                self.attach(spot, name, Some(image.bytes), cx);
            }
            PastedPicture::File(path) => {
                self.attach_file(spot, path, cx);
            }
        }
    }

    fn on_link(&self, cx: &Context<Self>) -> OnLink {
        let this = cx.weak_entity();
        Rc::new(move |target: &str, _: &mut Window, cx: &mut App| {
            let target = target.to_owned();
            this.update(cx, |this, cx| this.follow(&target, cx)).ok();
        })
    }

    fn want(&self, key: &str, source: MediaSource, size: Option<(u32, u32, bool)>) -> Media {
        let mut media = self.media.borrow_mut();
        if let Some(known) = media.get(key) {
            return known.clone();
        }
        media.insert(key.to_owned(), Media::Loading);
        self.command(Command::Fetch {
            key: key.to_owned(),
            source,
            size,
        });
        Media::Loading
    }

    fn face(
        &self,
        seed: &str,
        name: &str,
        picture: Option<&OwnedMxcUri>,
        size: Pixels,
    ) -> AnyElement {
        if let Some(uri) = picture
            && let Media::Ready(image) = self.want(
                &format!("{uri}#avatar"),
                MediaSource::Plain(uri.clone()),
                Some((96, 96, true)),
            )
        {
            return img(image)
                .size(size)
                .flex_none()
                .rounded_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element();
        }
        avatar(seed, name, size).into_any_element()
    }

    fn person_of(&self, id: &OwnedUserId, name: &str, avatar: Option<&OwnedMxcUri>) -> Person {
        Person {
            id: id.clone(),
            name: name.to_owned(),
            avatar: avatar.cloned(),
        }
    }

    fn person_hit(
        &self,
        id: impl Into<ElementId>,
        person: Person,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        div()
            .id(id.into())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_person(person.clone(), cx);
            }))
    }

    fn remove_space(&mut self, id: OwnedRoomId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self
            .summary(&id)
            .filter(|space| space.kind == RoomKind::Space && !space.invite)
        else {
            return;
        };
        self.removing_space = Some((id, space.name.clone()));
        self.show_dialog(Dialog::RemoveSpace, window, cx);
    }

    fn open_room_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.room.clone() {
            self.open_room_settings_for(id, window, cx);
        }
    }

    fn open_room_settings_for(
        &mut self,
        id: OwnedRoomId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(room) = self
            .summary(&id)
            .filter(|room| !room.invite && room.can_manage)
        else {
            return;
        };
        let (id, name, topic, can_name) = (
            room.id.clone(),
            room.explicit_name.clone().unwrap_or_default(),
            room.topic.clone().unwrap_or_default(),
            room.can_edit_name,
        );
        self.settings_room = Some(id.clone());
        self.admin.begin(cx);
        self.admin.loading = true;
        self.command(Command::LoadRoomAdmin(id));
        self.room_settings_busy = false;
        self.room_name
            .update(cx, |input, cx| input.set_text(name, cx));
        self.room_topic
            .update(cx, |input, cx| input.set_text(topic, cx));
        self.dialog = Some(Dialog::RoomSettings);
        focus(
            if can_name {
                &self.room_name
            } else {
                &self.room_topic
            },
            window,
            cx,
        );
        cx.notify();
    }

    fn save_room_settings(&mut self, cx: &mut Context<Self>) {
        if self.room_settings_busy {
            return;
        }
        let Some(room) = self.settings_room.as_ref().and_then(|id| self.summary(id)) else {
            self.notify("This room is no longer available.", true, cx);
            return;
        };
        let name = self.room_name.read(cx).text().trim().to_owned();
        let topic = self.room_topic.read(cx).text().trim().to_owned();
        let name = (name != room.explicit_name.as_deref().unwrap_or_default()).then_some(name);
        let topic = (topic != room.topic.as_deref().unwrap_or_default()).then_some(topic);
        if room.invite
            || (!room.can_edit_name && !room.can_edit_topic)
            || (name.is_some() && !room.can_edit_name)
            || (topic.is_some() && !room.can_edit_topic)
        {
            self.notify(
                "You don't have permission to change these room settings.",
                true,
                cx,
            );
            return;
        }
        if name.is_none() && topic.is_none() {
            self.close_dialog(cx);
            return;
        }
        let id = room.id.clone();
        self.room_settings_busy = true;
        self.command(Command::RoomSettings {
            room: id,
            name,
            topic,
        });
        cx.notify();
    }

    fn show_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Some(dialog);
        let hint = match dialog {
            Dialog::Join => "huddle-join-hint",
            Dialog::Create => "huddle-create-hint",
            Dialog::Talk => "huddle-talk-hint",
            Dialog::Choose
            | Dialog::Leave
            | Dialog::RemoveSpace
            | Dialog::Security
            | Dialog::RoomSettings
            | Dialog::Profile => "",
        };
        if dialog == Dialog::Profile {
            self.profile.begin(cx);
            self.command(Command::LoadProfile {
                room: self.room.clone(),
                ticket: self.profile.ticket,
            });
            self.profile_busy = false;
            self.profile_photo = None;
            self.profile_remove = false;
            if let Some(me) = &self.me {
                let name = me.name.clone();
                self.profile_name
                    .update(cx, |input, cx| input.set_text(name, cx));
            }
            focus(&self.profile_name, window, cx);
        }
        if dialog == Dialog::Security && !matches!(self.guard, Guard::Incoming | Guard::Emojis(_)) {
            self.guard = Guard::Menu;
        }
        if !hint.is_empty() {
            self.field.update(cx, |input, cx| {
                input.set_text("", cx);
                input.set_hint(hint, cx);
            });
            focus(&self.field, window, cx);
        }
        cx.notify();
    }

    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.dialog == Some(Dialog::Profile) && self.profile_busy {
            return;
        }
        if self.dialog == Some(Dialog::RoomSettings) && self.room_settings_busy {
            return;
        }
        if self.dialog == Some(Dialog::Security)
            && matches!(
                self.guard,
                Guard::Waiting | Guard::Incoming | Guard::Emojis(_)
            )
        {
            self.command(Command::CancelVerify);
        }
        self.dialog = None;
        self.settings_room = None;
        self.admin.reset();
        self.removing_space = None;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog else { return };
        let text = self.field.read(cx).text().trim().to_owned();
        let command =
            match dialog {
                Dialog::RoomSettings => {
                    self.save_room_settings(cx);
                    return;
                }
                Dialog::Choose => return,
                Dialog::Profile => {
                    self.save_profile(cx);
                    return;
                }
                Dialog::Security => {
                    if self.guard == Guard::Key && !text.is_empty() {
                        self.guard = Guard::Recovering;
                        self.command(Command::Recover(text));
                        cx.notify();
                    }
                    return;
                }
                Dialog::RemoveSpace => {
                    let Some((space, _)) = self.removing_space.take() else {
                        return;
                    };
                    self.dialog = None;
                    if !self.rooms.iter().any(|room| {
                        room.id == space && room.kind == RoomKind::Space && !room.invite
                    }) {
                        self.notify("That space is no longer joined.", true, cx);
                        return;
                    }
                    self.command(Command::Leave(space));
                    self.notify("Removing space…", false, cx);
                    return;
                }
                Dialog::Leave => match self.room.clone() {
                    Some(room) => {
                        self.stop_typing();
                        self.room = None;
                        self.thread = None;
                        self.list.reset(0);
                        self.dialog = None;
                        Command::Leave(room)
                    }
                    None => return,
                },
                _ if text.is_empty() => return,
                Dialog::Join => Command::Join(text),
                Dialog::Create => Command::Create(text),
                Dialog::Talk => Command::Talk(text),
            };
        self.command(command);
        if dialog != Dialog::Leave {
            self.notify(
                match dialog {
                    Dialog::Join => "Joining…",
                    Dialog::Create => "Creating your room…",
                    _ => "Starting the conversation…",
                },
                false,
                cx,
            );
            self.dialog = None;
        }
        cx.notify();
    }
}

fn focus(input: &Entity<Input>, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| input.focus(window, cx));
}

fn mime_of(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::Webp => "image/webp",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Svg => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn measure(bytes: &[u8]) -> (u32, u32) {
    png_size(bytes)
        .or_else(|| gif_size(bytes))
        .or_else(|| bmp_size(bytes))
        .or_else(|| webp_size(bytes))
        .or_else(|| jpeg_size(bytes))
        .unwrap_or((0, 0))
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short = flat.chars().take(140).collect::<String>();
    if flat.chars().count() > 140 {
        short.push('…');
    }
    short
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 24 && &bytes[12..16] == b"IHDR" {
        Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        ))
    } else {
        None
    }
}

fn gif_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 10 && bytes.starts_with(b"GIF8") {
        Some((
            u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
            u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
        ))
    } else {
        None
    }
}

fn bmp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 26 && bytes.starts_with(b"BM") {
        Some((
            i32::from_le_bytes(bytes[18..22].try_into().ok()?).unsigned_abs(),
            i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs(),
        ))
    } else {
        None
    }
}

fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8 " if bytes[23..26] == [0x9d, 0x01, 0x2a] => Some((
            u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3fff,
            u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3fff,
        )),
        b"VP8L" if bytes[20] == 0x2f => {
            let packed = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
            Some(((packed & 0x3fff) + 1, ((packed >> 14) & 0x3fff) + 1))
        }
        b"VP8X" => Some((
            1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]),
            1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]),
        )),
        _ => None,
    }
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut index = 2;
    while index + 8 < bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        while index < bytes.len() && bytes[index] == 0xFF {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let marker = bytes[index];
        index += 1;
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if index + 1 >= bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
        if len < 2 || index + len > bytes.len() {
            return None;
        }
        if matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        ) && len >= 7
        {
            return Some((
                u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32,
                u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]) as u32,
            ));
        }
        index += len;
    }
    None
}

fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, 0xFF, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some(ImageFormat::Webp),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        _ if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") => Some(ImageFormat::Svg),
        _ => None,
    }
}

/// Slides a side panel open or shut. The same element keeps its spring, so showing
/// and hiding continues from the width it already has. Reduced motion settles at once.
fn slide(
    child: AnyElement,
    name: &'static str,
    open: bool,
    width: Pixels,
    enter: bool,
) -> AnyElement {
    let target = if open { width.as_f32() } else { 0. };
    let mut spring = SpringAnimation::new(ui::Springs::RESPONSIVE).to(target);
    if enter {
        spring = spring.from(0.);
    }
    div()
        .flex_none()
        .h_full()
        .min_w(px(0.))
        .overflow_hidden()
        .child(child)
        .with_spring(name, spring, |shell, width| {
            let width = px(width.max(0.));
            shell.w(width).min_w(px(0.)).max_w(width)
        })
        .into_any_element()
}

fn fit(width: u32, height: u32) -> (f32, f32) {
    if width == 0 || height == 0 {
        return (320., 200.);
    }
    let (width, height) = (width as f32, height as f32);
    let scale = (400. / width).min(300. / height).min(1.);
    ((width * scale).max(48.), (height * scale).max(48.))
}

fn icon(path: &str, size: Pixels, color: Hsla) -> impl IntoElement {
    svg()
        .path(icons::path(path))
        .size(size)
        .flex_none()
        .text_color(color)
}

fn green() -> Hsla {
    gpui::hsla(0.39, 0.6, 0.45, 1.)
}

fn amber() -> Hsla {
    gpui::hsla(0.11, 0.85, 0.55, 1.)
}

fn hue(seed: &str) -> Hsla {
    let hash = seed.bytes().fold(5381u32, |hash, byte| {
        hash.wrapping_mul(33) ^ u32::from(byte)
    });
    Hsla {
        h: (hash % 360) as f32 / 360.,
        s: 0.55,
        l: 0.5,
        a: 1.,
    }
}

fn initials(name: &str) -> String {
    let words = name
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .collect::<String>();
    match words.is_empty() {
        true => "?".into(),
        false => words.to_uppercase(),
    }
}

fn avatar(seed: &str, name: &str, size: Pixels) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(hue(seed))
        .text_color(gpui::white())
        .text_size(size * 0.4)
        .font_weight(FontWeight::SEMIBOLD)
        .child(initials(name).chars().next().unwrap_or('?').to_string())
}

fn local(ms: i64) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

fn day(ms: i64) -> Option<NaiveDate> {
    local(ms).map(|time| time.date_naive())
}

fn stamp(ms: i64) -> String {
    let Some(time) = local(ms) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    match time.date_naive() {
        date if date == today => format!("Today at {}", time.format("%H:%M")),
        date if Some(date) == today.pred_opt() => format!("Yesterday at {}", time.format("%H:%M")),
        _ => time.format("%d/%m/%Y %H:%M").to_string(),
    }
}

fn clock(ms: i64) -> String {
    local(ms).map_or_else(String::new, |time| time.format("%H:%M").to_string())
}

fn day_label(ms: i64) -> String {
    local(ms).map_or_else(String::new, |time| time.format("%B %-d, %Y").to_string())
}

fn message_key(message: &Message) -> String {
    match (&message.id, message.local) {
        (Some(id), _) => id.to_string(),
        (None, Some(local)) => format!("local-{local}"),
        (None, None) => "unknown".into(),
    }
}

impl MatrixClient {
    fn render_starting(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        div()
            .flex()
            .flex_1()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(theme.muted_foreground)
            .child(icon("icons/huddle.svg", px(40.), theme.foreground))
            .child("Connecting to Matrix…")
            .into_any_element()
    }

    fn render_sign_in(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let signing_up = self.signing_up;
        let busy = match signing_up {
            true => self.signup_busy,
            false => self.phase == Phase::SigningIn,
        };
        let label = |text: &'static str| {
            div()
                .text_size(theme.text(Text::Small))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let field = |name: &'static str, input: &Entity<Input>| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(label(name))
                .child(div().flex().h(px(40.)).child(input.clone()))
        };
        let small = |text: String| {
            div()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let link = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .text_color(theme.primary)
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(text)
        };
        let server = self.server.read(cx).text().trim().to_owned();
        let server = if server.is_empty() {
            "matrix.org".to_owned()
        } else {
            server
        };

        let (title, detail) = match (signing_up, &self.step) {
            (false, _) => (
                "Welcome to Huddle",
                "Chat with friends and communities on Matrix, an open network that no single company owns.",
            ),
            (true, None) => (
                "Create your account",
                "Pick a username and password. You can change your display name later.",
            ),
            (true, Some(Step::Token)) => (
                "Enter your invite code",
                "This server only lets people with an invite code join. Ask whoever runs it for one.",
            ),
            (true, Some(Step::Terms(_))) => (
                "Accept the rules",
                "This server asks you to agree to its policies before joining.",
            ),
            (true, Some(Step::Captcha(_))) => (
                "Quick check",
                "This server wants to make sure you're a person. We'll open the check in your browser.",
            ),
            (true, Some(Step::Email)) => (
                "Add your email",
                "This server needs an email address to create your account.",
            ),
            (true, Some(Step::EmailSent(_))) => (
                "Check your inbox",
                "Open the link in the email we sent, then come back here.",
            ),
            (true, Some(Step::Website(_))) => (
                "Sign up on the website",
                "This server creates accounts on its own website. Once you're done, come back and sign in here.",
            ),
        };

        let mut card = div()
            .flex()
            .flex_col()
            .gap_5()
            .w(px(440.))
            .max_w_full()
            .p_8()
            .rounded(theme.radius * 2.)
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(icon("icons/huddle.svg", px(44.), theme.primary))
                    .child(
                        div()
                            .text_size(theme.text(Text::Title))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(div().text_color(theme.muted_foreground).child(detail)),
            );

        let server_choice = match self.choosing_server {
            true => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(field("SERVER", &self.server))
                .child(small(
                    "Your server is the part after the colon in your Matrix ID. Most people use matrix.org.".into(),
                )),
            false => div()
                .flex()
                .flex_wrap()
                .gap_1()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(match signing_up {
                    true => format!("Creating an account on {server}."),
                    false => format!("Signing in on {server}."),
                })
                .child(
                    div()
                        .id("change-server")
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .child("Use a different server")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.choosing_server = true;
                            focus(&this.server, window, cx);
                            cx.notify();
                        })),
                ),
        };

        let problem = self.sign_in_error.clone().map(|problem| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .p_3()
                .rounded(theme.radius)
                .bg(theme.danger.opacity(0.12))
                .text_color(theme.danger)
                .text_size(theme.text(Text::Small))
                .child(icon("icons/circle-alert.svg", px(16.), theme.danger))
                .child(div().flex_1().child(problem))
        });

        let back_to_form = link("signup-back", "Go back")
            .on_click(cx.listener(|this, _, _, cx| this.leave_sign_up(cx)));

        match (signing_up, self.step.clone()) {
            (false, _) => {
                card = card
                    .on_action(cx.listener(|this, _: &Submit, window, cx| this.sign_in(window, cx)))
                    .child(field("USERNAME", &self.username))
                    .child(field("PASSWORD", &self.password))
                    .child(server_choice)
                    .children(problem)
                    .child(
                        Button::new("sign-in")
                            .primary()
                            .w_full()
                            .h(px(42.))
                            .label(if busy { "Signing in…" } else { "Sign in" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx))),
                    )
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.muted_foreground)
                            .child("New to Matrix?")
                            .child(
                                link("sign-up", "Create a free account").on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.signing_up = true;
                                        this.sign_in_error = None;
                                        focus(&this.username, window, cx);
                                        cx.notify();
                                    },
                                )),
                            ),
                    );
            }
            (true, None) => {
                card = card
                    .on_action(cx.listener(|this, _: &Submit, window, cx| this.sign_up(window, cx)))
                    .child(field("USERNAME", &self.username))
                    .child(field("PASSWORD", &self.password))
                    .child(field("CONFIRM PASSWORD", &self.confirm_password))
                    .child(server_choice)
                    .children(problem)
                    .child(
                        Button::new("sign-up-submit")
                            .primary()
                            .w_full()
                            .h(px(42.))
                            .label(if busy {
                                "Creating your account…"
                            } else {
                                "Create account"
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.sign_up(window, cx))),
                    )
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.muted_foreground)
                            .child("Already have an account?")
                            .child(link("back-to-sign-in", "Sign in").on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.signing_up = false;
                                    this.sign_in_error = None;
                                    focus(&this.username, window, cx);
                                    cx.notify();
                                },
                            ))),
                    );
            }
            (true, Some(step)) => {
                let body = match step {
                    Step::Token | Step::Email => {
                        let send = matches!(step, Step::Email);
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .on_action(cx.listener(|this, _: &Submit, _, cx| this.submit_step(cx)))
                            .child(field(
                                if send { "EMAIL" } else { "INVITE CODE" },
                                &self.step_field,
                            ))
                            .children(problem)
                            .child(
                                Button::new("signup-step")
                                    .primary()
                                    .w_full()
                                    .h(px(42.))
                                    .label(match (busy, send) {
                                        (true, _) => "Checking…",
                                        (false, true) => "Send link",
                                        (false, false) => "Continue",
                                    })
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.submit_step(cx))),
                            )
                    }
                    Step::Terms(policies) => div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .children(
                            policies
                                .into_iter()
                                .enumerate()
                                .map(|(index, (name, url))| {
                                    div()
                                        .id(SharedString::from(format!("policy-{index}")))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_color(theme.primary)
                                        .cursor_pointer()
                                        .hover(|style| style.underline())
                                        .child(icon("icons/link.svg", px(14.), theme.primary))
                                        .child(name)
                                        .on_click(move |_, _, cx| cx.open_url(&url))
                                }),
                        )
                        .child(
                            Checkbox::new("signup-agree", self.agreed)
                                .label("I agree to these policies")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.agreed = !this.agreed;
                                    cx.notify();
                                })),
                        )
                        .children(problem)
                        .child(
                            Button::new("signup-terms")
                                .primary()
                                .w_full()
                                .h(px(42.))
                                .label(if busy { "Checking…" } else { "Continue" })
                                .disabled(busy || !self.agreed)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer(Answer::Terms, cx)),
                                ),
                        ),
                    Step::Captcha(url) => div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .children(problem)
                        .child(
                            Button::new("signup-captcha-open")
                                .primary()
                                .w_full()
                                .h(px(42.))
                                .label("Open the check")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(
                            Button::new("signup-captcha-done")
                                .secondary()
                                .w_full()
                                .h(px(42.))
                                .label(if busy { "Checking…" } else { "I've done it" })
                                .disabled(busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer(Answer::Captcha, cx)),
                                ),
                        ),
                    Step::EmailSent(address) => {
                        let again = address.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(small(format!("We sent it to {address}.")))
                            .children(problem)
                            .child(
                                Button::new("signup-email-done")
                                    .primary()
                                    .w_full()
                                    .h(px(42.))
                                    .label(if busy {
                                        "Checking…"
                                    } else {
                                        "I've opened the link"
                                    })
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.answer(Answer::EmailConfirmed, cx)
                                    })),
                            )
                            .child(
                                Button::new("signup-email-again")
                                    .ghost()
                                    .w_full()
                                    .label("Send it again")
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer(Answer::Email(again.clone()), cx)
                                    })),
                            )
                    }
                    Step::Website(url) => div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            Button::new("signup-website")
                                .primary()
                                .w_full()
                                .h(px(42.))
                                .label("Open the sign-up page")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(
                            Button::new("signup-then-sign-in")
                                .secondary()
                                .w_full()
                                .h(px(42.))
                                .label("I have an account now")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.leave_sign_up(cx);
                                    this.signing_up = false;
                                    focus(&this.username, window, cx);
                                })),
                        ),
                };
                card = card.child(body).child(
                    div()
                        .flex()
                        .justify_center()
                        .text_size(theme.text(Text::Small))
                        .child(back_to_form),
                );
            }
        }

        div()
            .id("sign-in-page")
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_6()
            .overflow_y_scroll()
            .child(card)
            .into_any_element()
    }

    fn rail_hover(&mut self, key: &str, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.rail_hover.as_deref() == Some(key) {
                return;
            }
            self.rail_hover = Some(key.to_owned());
        } else if self.rail_hover.as_deref() == Some(key) {
            self.rail_hover = None;
        } else {
            return;
        }
        cx.notify();
    }

    fn save_rail_groups(&mut self, cx: &mut Context<Self>) {
        if let Some(me) = &self.me
            && let Err(error) = self.rail_groups.save(me.id.as_str())
        {
            log::warn!("Could not save navigation groups: {error:#}");
            self.notify("Couldn't save groups. Try again.", true, cx);
        }
        cx.notify();
    }

    fn render_rail_group(
        &self,
        group: &rail_groups::Group,
        members: &[&RoomSummary],
        children: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let id = group.id;
        let expanded = group.expanded;
        let hover_key = format!("rail-group-{id}");
        let hovered = self.rail_hover.as_deref() == Some(&hover_key);
        let names = members
            .iter()
            .map(|room| room.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let unread = unread::group(&self.rooms, group.rooms.iter());
        let active = members.iter().any(|room| {
            self.space.as_ref() == Some(&room.id)
                || (self.space.is_none() && self.room.as_ref() == Some(&room.id))
        });
        // Keep the body mounted in both states so closing can finish, and rapid
        // reversals preserve the spring's current height and velocity.
        let body_height =
            56. * children.len() as f32 + 8. + theme.metrics.control_small.as_f32() + 2.;
        let open = if expanded { 1. } else { 0. };
        let header = div()
            .id(SharedString::from(hover_key.clone()))
            .relative()
            .size(px(48.))
            .flex_none()
            .rounded(px(16.))
            .cursor_pointer()
            .tip_label(
                format!(
                    "{names} · {}",
                    if expanded {
                        "Collapse group"
                    } else {
                        "Expand group"
                    }
                ),
                Perch::Right,
            )
            .on_hover(cx.listener({
                let key = hover_key.clone();
                move |this, hovered: &bool, _, cx| this.rail_hover(&key, *hovered, cx)
            }))
            .on_drag_move(cx.listener({
                let key = hover_key.clone();
                move |this, event: &gpui::DragMoveEvent<rail_groups::DraggedRoom>, _, cx| {
                    this.rail_hover(&key, event.bounds.contains(&event.event.position), cx);
                }
            }))
            .on_drop(
                cx.listener(move |this, dragged: &rail_groups::DraggedRoom, _, cx| {
                    if this.rail_groups.move_into(dragged.room.clone(), id) {
                        this.save_rail_groups(cx);
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(group) = this
                    .rail_groups
                    .groups
                    .iter_mut()
                    .find(|group| group.id == id)
                {
                    group.expanded = !group.expanded;
                    this.save_rail_groups(cx);
                }
            }))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("icons/folder-plus.svg", px(24.), theme.primary))
                    .with_spring(
                        SharedString::from(format!("folder-art-{id}")),
                        SpringAnimation::new(ui::Springs::RESPONSIVE).to(open),
                        |art, amount| {
                            art.opacity(amount.clamp(0., 1.))
                                .layer_scale(0.9 + 0.1 * amount.clamp(0., 1.))
                        },
                    ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .p_1()
                    .flex()
                    .flex_wrap()
                    .gap(px(2.))
                    .overflow_hidden()
                    .children(members.iter().take(4).map(|room| {
                        self.face(room.id.as_str(), &room.name, room.avatar.as_ref(), px(18.))
                    }))
                    .with_spring(
                        SharedString::from(format!("folder-preview-{id}")),
                        SpringAnimation::new(ui::Springs::RESPONSIVE).to(open),
                        |preview, amount| {
                            preview
                                .opacity(1. - amount.clamp(0., 1.))
                                .layer_scale(1. - 0.08 * amount.clamp(0., 1.))
                        },
                    ),
            )
            .child(
                div()
                    .absolute()
                    .right(px(-3.))
                    .top(px(-3.))
                    .rounded_full()
                    .bg(theme.sidebar)
                    .child(
                        svg()
                            .path(icons::path("icons/chevron-right.svg"))
                            .size(px(12.))
                            .text_color(theme.primary)
                            .with_spring(
                                SharedString::from(format!("folder-chevron-{id}")),
                                SpringAnimation::new(ui::Springs::RESPONSIVE).to(open),
                                |chevron, amount| {
                                    chevron.with_transformation(gpui::Transformation::rotate(
                                        gpui::radians(std::f32::consts::FRAC_PI_2 * amount),
                                    ))
                                },
                            ),
                    ),
            )
            .when(!expanded && unread > 0, |header| {
                header.child(
                    unread::badge(unread, cx)
                        .absolute()
                        .right(px(-5.))
                        .bottom(px(-3.))
                        .border_2()
                        .border_color(theme.sidebar),
                )
            })
            .with_spring(
                SharedString::from(format!("folder-hover-{id}")),
                SpringAnimation::new(ui::Springs::RESPONSIVE).to(if hovered { 1. } else { 0. }),
                move |header, amount| {
                    header
                        .bg(theme.primary.opacity(0.15 + 0.2 * amount.clamp(0., 1.)))
                        .layer_scale(1. + 0.04 * amount.clamp(0., 1.))
                },
            );
        let body = div()
            .w_full()
            .flex_none()
            .min_h(px(0.))
            .overflow_hidden()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .children(children)
                    .child(
                        Button::new(SharedString::from(format!("rail-group-remove-{id}")))
                            .ghost()
                            .small()
                            .icon("icons/x.svg")
                            .tooltip("Ungroup rooms and spaces")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rail_groups.groups.retain(|group| group.id != id);
                                this.save_rail_groups(cx);
                            })),
                    ),
            )
            .with_spring(
                SharedString::from(format!("folder-body-{id}")),
                SpringAnimation::new(ui::Springs::RESPONSIVE).to(if expanded {
                    body_height
                } else {
                    0.
                }),
                move |body, height| {
                    body.h(px(height.max(0.)))
                        .opacity((height / body_height).clamp(0., 1.))
                },
            );
        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .py_1()
            .rounded(px(18.))
            .bg(theme.primary.opacity(if active { 0.14 } else { 0.06 }))
            .child(header)
            .child(body)
            .motion(
                SharedString::from(format!("folder-enter-{id}")),
                ui::motion::Motion::Quick,
                |folder, amount| folder.opacity(amount).layer_scale(0.96 + 0.04 * amount),
            )
            .into_any_element()
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let home_count = unread::home(&self.rooms);
        let home_unread = home_count > 0 || self.rooms.iter().any(|room| room.invite);
        type Press = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
        let item = |id: SharedString,
                    name: SharedString,
                    room: Option<OwnedRoomId>,
                    active: bool,
                    unread: bool,
                    count: u64,
                    content: AnyElement,
                    press: Press| {
            let hover_key = id.to_string();
            // Closing folders keep their children mounted for the exit animation;
            // those clipped icons must not react to a drag over the rows below.
            let drag_visible = room
                .as_ref()
                .and_then(|room| self.rail_groups.group_of(room))
                .and_then(|id| self.rail_groups.groups.iter().find(|group| group.id == id))
                .is_none_or(|group| group.expanded);
            let hovered = self.rail_hover.as_deref() == Some(&hover_key);
            let background = if active {
                theme.primary
            } else {
                theme.secondary
            };
            let foreground = if active {
                theme.primary_foreground
            } else {
                theme.foreground
            };
            div()
                .relative()
                .flex_none()
                .flex()
                .justify_center()
                .w_full()
                .child({
                    let height = if active {
                        40.
                    } else if unread {
                        10.
                    } else {
                        0.
                    };
                    div()
                        .absolute()
                        .left_0()
                        .w(px(4.))
                        .rounded_r(px(4.))
                        .bg(theme.foreground)
                        .motion(
                            SharedString::from(format!(
                                "rail-mark-{id}-{}-{}",
                                u8::from(active),
                                u8::from(unread)
                            )),
                            ui::motion::Motion::Quick,
                            move |mark, progress| {
                                let (top, size) = if active {
                                    (19. - 15. * progress, 8. + (height - 8.) * progress)
                                } else if unread {
                                    (19., height * progress)
                                } else {
                                    (19., 10. * (1. - progress))
                                };
                                mark.top(px(top)).h(px(size.max(0.)))
                            },
                        )
                })
                .child(
                    div()
                        .id(id.clone())
                        .on_hover(cx.listener({
                            let key = hover_key.clone();
                            move |this, hovered: &bool, _, cx| this.rail_hover(&key, *hovered, cx)
                        }))
                        .tip_label(name.clone(), Perch::Right)
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(48.))
                        .rounded_full()
                        .bg(if active {
                            theme.primary
                        } else {
                            theme.secondary
                        })
                        .text_color(if active {
                            theme.primary_foreground
                        } else {
                            theme.foreground
                        })
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size_full()
                                .overflow_hidden()
                                .rounded_full()
                                .child(content),
                        )
                        .when(count > 0, |item| {
                            item.child(
                                unread::badge(count, cx)
                                    .absolute()
                                    .right(px(-5.))
                                    .bottom(px(-3.))
                                    .border_2()
                                    .border_color(theme.sidebar),
                            )
                        })
                        .when_some(room, |icon, room| {
                            let target = room.clone();
                            let hover_target = room.clone();
                            icon.on_drag(
                                rail_groups::DraggedRoom {
                                    room,
                                    name: name.to_string(),
                                },
                                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                            )
                            .on_drag_move(cx.listener({
                                let key = hover_key.clone();
                                move |this,
                                      event: &gpui::DragMoveEvent<rail_groups::DraggedRoom>,
                                      _,
                                      cx| {
                                    let over = drag_visible
                                        && event.bounds.contains(&event.event.position)
                                        && event.drag(cx).room != hover_target;
                                    this.rail_hover(&key, over, cx);
                                }
                            }))
                            .on_drop(cx.listener({
                                let target = target.clone();
                                move |this, dragged: &rail_groups::DraggedRoom, _, cx| {
                                    if this
                                        .rail_groups
                                        .combine(dragged.room.clone(), target.clone())
                                    {
                                        this.save_rail_groups(cx);
                                    }
                                }
                            }))
                        })
                        .on_click(press)
                        .with_spring(
                            SharedString::from(format!("{id}-hover")),
                            SpringAnimation::new(ui::Springs::RESPONSIVE).to(if hovered {
                                1.
                            } else {
                                0.
                            }),
                            move |icon, amount| {
                                let amount = amount.clamp(0., 1.);
                                icon.rounded(px(24. - 8. * amount))
                                    .bg(ui::motion::mix(background, theme.primary, amount))
                                    .text_color(ui::motion::mix(
                                        foreground,
                                        theme.primary_foreground,
                                        amount,
                                    ))
                                    .layer_scale(1. + 0.04 * amount)
                            },
                        ),
                )
        };

        let grouped = self
            .rooms
            .iter()
            .filter(|room| room.kind == RoomKind::Space && !room.invite)
            .flat_map(|room| room.children.iter().cloned())
            .collect::<Vec<_>>();
        let loose = self
            .rooms
            .iter()
            .filter(|room| {
                !room.invite
                    && ((room.kind == RoomKind::Room && !grouped.contains(&room.id))
                        || (room.kind != RoomKind::Space
                            && self.rail_groups.group_of(&room.id).is_some()))
            })
            .map(|room| {
                (
                    room.id.clone(),
                    room.name.clone(),
                    room.avatar.clone(),
                    room.unread,
                )
            })
            .collect::<Vec<_>>();
        let home_active = self.on_home();

        let home = item(
            "rail-home".into(),
            "Home · drag here to ungroup".into(),
            None,
            home_active,
            home_unread,
            home_count,
            svg()
                .path(icons::path("icons/house.svg"))
                .size(px(22.))
                .text_color(if home_active {
                    theme.primary_foreground
                } else {
                    theme.foreground
                })
                .into_any_element(),
            Box::new(cx.listener(|this, _, _, cx| {
                let leave = this.room.as_ref().is_some_and(|id| this.loose(id));
                this.space = None;
                this.space_trail.clear();
                if leave {
                    this.stop_typing();
                    this.room = None;
                }
                cx.notify();
            })),
        );

        let rooms = loose
            .into_iter()
            .map(|(id, name, avatar, unread)| {
                let picture = avatar.as_ref().and_then(|uri| {
                    match self.want(
                        &format!("{uri}#avatar"),
                        MediaSource::Plain(uri.clone()),
                        Some((96, 96, true)),
                    ) {
                        Media::Ready(image) => Some(image),
                        _ => None,
                    }
                });
                let open = id.clone();
                let element = item(
                    SharedString::from(format!("rail-room-{}", id)),
                    name.clone().into(),
                    Some(id.clone()),
                    self.space.is_none() && self.room.as_ref() == Some(&id),
                    unread > 0,
                    unread,
                    match picture {
                        Some(image) => img(image)
                            .size(px(48.))
                            .rounded_full()
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => div().child(initials(&name)).into_any_element(),
                    },
                    Box::new(cx.listener(move |this, _, window, cx| {
                        this.space = None;
                        this.space_trail.clear();
                        this.open(open.clone(), window, cx);
                    })),
                );
                (id, element.into_any_element())
            })
            .collect::<Vec<_>>();

        let spaces = self
            .rooms
            .iter()
            .filter(|room| room.kind == RoomKind::Space && !room.invite)
            .map(|space| {
                let id = space.id.clone();
                let unread = unread::space(&self.rooms, &space.id);
                let picture = space.avatar.as_ref().and_then(|uri| {
                    match self.want(
                        &format!("{uri}#avatar"),
                        MediaSource::Plain(uri.clone()),
                        Some((96, 96, true)),
                    ) {
                        Media::Ready(image) => Some(image),
                        _ => None,
                    }
                });
                let element = item(
                    SharedString::from(format!("rail-{}", space.id)),
                    space.name.clone().into(),
                    Some(space.id.clone()),
                    self.space.as_ref() == Some(&space.id),
                    unread > 0,
                    unread,
                    match picture {
                        Some(image) => img(image)
                            .size(px(48.))
                            .rounded_full()
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => div().child(initials(&space.name)).into_any_element(),
                    },
                    Box::new(cx.listener(move |this, _, _, cx| {
                        this.enter_space(id.clone(), false, cx);
                    })),
                );
                (space.id.clone(), element.into_any_element())
            })
            .collect::<Vec<_>>();

        let order: Vec<_> = rooms
            .iter()
            .chain(&spaces)
            .map(|(id, _)| id.clone())
            .collect();
        let mut icons: HashMap<_, _> = rooms.into_iter().chain(spaces).collect();
        let mut entries = Vec::new();
        let available: std::collections::HashSet<_> = order.iter().cloned().collect();
        for id in order {
            if let Some(group_id) = self.rail_groups.group_of(&id) {
                let group = self
                    .rail_groups
                    .groups
                    .iter()
                    .find(|group| group.id == group_id)
                    .unwrap();
                // Keep the folder at the target icon's position, even when its source came first.
                if group
                    .rooms
                    .iter()
                    .find(|member| available.contains(*member))
                    != Some(&id)
                {
                    continue;
                }
                let members = group
                    .rooms
                    .iter()
                    .filter_map(|id| self.summary(id).filter(|room| !room.invite))
                    .collect::<Vec<_>>();
                let children = group
                    .rooms
                    .iter()
                    .filter_map(|id| icons.remove(id))
                    .collect();
                entries.push(self.render_rail_group(group, &members, children, cx));
            } else if let Some(icon) = icons.remove(&id) {
                entries.push(icon);
            }
        }

        div()
            .id("rail")
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap_2()
            .w(RAIL)
            .h_full()
            .py_3()
            .bg(theme.sidebar)
            .overflow_y_scroll()
            .child(
                div()
                    .id("rail-ungroup")
                    .w_full()
                    .tip_label("Drag here to ungroup", Perch::Right)
                    .drag_over::<rail_groups::DraggedRoom>(move |style, _, _, _| {
                        style.bg(theme.primary.opacity(0.15)).rounded(theme.radius)
                    })
                    .on_drop(
                        cx.listener(|this, dragged: &rail_groups::DraggedRoom, _, cx| {
                            if this.rail_groups.ungroup(&dragged.room) {
                                this.save_rail_groups(cx);
                            }
                        }),
                    )
                    .child(home),
            )
            .child(
                div()
                    .w(px(32.))
                    .h(px(2.))
                    .my_1()
                    .rounded_full()
                    .bg(theme.border),
            )
            .children(entries)
            .child(
                div()
                    .id("rail-add")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(48.))
                    .rounded_full()
                    .bg(theme.secondary)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.primary.opacity(0.2)))
                    .child(icon("icons/plus.svg", px(22.), theme.primary))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_dialog(Dialog::Choose, window, cx)
                    })),
            )
    }

    fn render_room_row(&self, room: &RoomSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let active = self.room.as_ref() == Some(&room.id);
        let unread = room.unread > 0;
        let id = room.id.clone();
        let leading = match room.kind {
            RoomKind::Direct => {
                self.face(room.id.as_str(), &room.name, room.avatar.as_ref(), px(26.))
            }
            _ => div()
                .w(px(26.))
                .flex()
                .justify_center()
                .text_size(px(20.))
                .text_color(theme.muted_foreground)
                .child("#")
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("room-{}", room.id)))
            .tip_label(room.name.clone(), Perch::Right)
            .on_drag(
                rail_groups::DraggedRoom {
                    room: id.clone(),
                    name: room.name.clone(),
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .drag_over::<rail_groups::DraggedRoom>(move |style, _, _, _| {
                style.bg(theme.primary.opacity(0.2))
            })
            .on_drop(cx.listener({
                let target = id.clone();
                move |this, dragged: &rail_groups::DraggedRoom, _, cx| {
                    if this
                        .rail_groups
                        .combine(dragged.room.clone(), target.clone())
                    {
                        this.save_rail_groups(cx);
                    }
                }
            }))
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.))
            .px_2()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_color(if active || unread {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .when_else(
                active,
                |row| row.bg(theme.sidebar_accent),
                |row| {
                    row.hover(|style| style.bg(theme.secondary_hover).text_color(theme.foreground))
                },
            )
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(unread, |name| name.font_weight(FontWeight::SEMIBOLD))
                    .child(room.name.clone()),
            )
            .when(room.encrypted && room.kind != RoomKind::Direct, |row| {
                row.child(icon("icons/lock.svg", px(12.), theme.muted_foreground))
            })
            .when(unread, |row| row.child(unread::badge(room.unread, cx)))
            .on_click(cx.listener(move |this, _, window, cx| this.open(id.clone(), window, cx)))
            .into_any_element()
    }

    fn render_invite(&self, room: &RoomSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let accept = room.id.clone();
        let decline = room.id.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded(theme.radius)
            .bg(theme.primary.opacity(0.08))
            .border_1()
            .border_color(theme.primary.opacity(0.3))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.face(room.id.as_str(), &room.name, room.avatar.as_ref(), px(26.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(room.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("accept-{}", room.id)))
                            .primary()
                            .small()
                            .flex_1()
                            .label("Accept")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.command(Command::Accept(accept.clone()));
                                this.notify("Joining…", false, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("decline-{}", room.id)))
                            .outline()
                            .small()
                            .flex_1()
                            .label("Decline")
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.command(Command::Leave(decline.clone()));
                            })),
                    ),
            )
            .into_any_element()
    }

    fn section(&self, title: &'static str, add: Option<Dialog>, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        div()
            .flex()
            .items_center()
            .justify_between()
            .pt_4()
            .pb_1()
            .px_2()
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .text_color(theme.muted_foreground)
            .child(title)
            .when_some(add, |header, dialog| {
                header.child(
                    div()
                        .id(SharedString::from(format!("section-add-{title}")))
                        .cursor_pointer()
                        .rounded(px(4.))
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(icon("icons/plus.svg", px(14.), theme.muted_foreground))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_dialog(dialog, window, cx)
                        })),
                )
            })
    }

    fn load_linked_names(&mut self) {
        let Some(space) = self.space.as_ref().and_then(|id| self.summary(id)) else {
            return;
        };
        if !space
            .children
            .iter()
            .any(|id| id != &space.id && self.summary(id).is_none_or(|room| room.invite))
        {
            return;
        }
        if self
            .linked_name_requests
            .get(&space.id)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(300))
        {
            return;
        }
        let id = space.id.clone();
        self.linked_name_requests.insert(id.clone(), Instant::now());
        self.command(Command::LoadSpaceNames(id));
    }

    fn animate_space_navigation(&mut self, backwards: bool) {
        self.space_transition = self.space_transition.wrapping_add(1);
        self.space_backwards = backwards;
        self.rail_hover = None;
    }

    fn enter_space(&mut self, id: OwnedRoomId, nested: bool, cx: &mut Context<Self>) {
        if !self
            .summary(&id)
            .is_some_and(|room| room.kind == RoomKind::Space && !room.invite)
        {
            return;
        }
        self.space_trail
            .enter(&self.rooms, self.space.as_ref(), &id, nested);
        if self.space.as_ref() != Some(&id) {
            self.animate_space_navigation(false);
        }
        self.space = Some(id);
        self.load_linked_names();
        cx.notify();
    }

    fn render_subspace(&self, space: &RoomSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let id = space.id.clone();
        let unread = unread::space(&self.rooms, &id);
        let hover_key = format!("subspace-{id}");
        let hovered = self.rail_hover.as_deref() == Some(&hover_key);
        let hover_target = if hovered { 1. } else { 0. };
        let arrow_id = SharedString::from(format!("subspace-arrow-{id}"));
        let hover_id = SharedString::from(format!("subspace-hover-{id}"));
        div()
            .id(SharedString::from(hover_key.clone()))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.rail_hover(&hover_key, *hovered, cx);
            }))
            .flex()
            .items_center()
            .gap_2()
            .h(px(40.))
            .px_2()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_color(theme.foreground)
            .tip_label(space.name.clone(), Perch::Right)
            .child(self.face(id.as_str(), &space.name, space.avatar.as_ref(), px(26.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(space.name.clone()),
            )
            .when(unread > 0, |row| row.child(unread::badge(unread, cx)))
            .child(
                div()
                    .flex_none()
                    .child(icon(
                        "icons/chevron-right.svg",
                        px(14.),
                        theme.muted_foreground,
                    ))
                    .with_spring(
                        arrow_id,
                        SpringAnimation::new(ui::Springs::RESPONSIVE).to(hover_target),
                        |arrow, amount| {
                            arrow.layer_translate(point(px(3. * amount.clamp(0., 1.)), px(0.)))
                        },
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.enter_space(id.clone(), true, cx)))
            .with_spring(
                hover_id,
                SpringAnimation::new(ui::Springs::RESPONSIVE).to(hover_target),
                move |row, amount| row.bg(theme.secondary_hover.opacity(amount.clamp(0., 1.))),
            )
            .into_any_element()
    }

    fn render_channels(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let space = self.space.as_ref().and_then(|space| self.summary(space));
        let (invites, rooms, directs): (Vec<_>, Vec<_>, Vec<_>) = match space {
            Some(space) => (Vec::new(), spaces::children(&self.rooms, space), Vec::new()),
            None => (
                self.rooms.iter().filter(|room| room.invite).collect(),
                self.rooms
                    .iter()
                    .filter(|room| {
                        room.kind == RoomKind::Room
                            && !room.invite
                            && self.space_of(&room.id).is_none()
                    })
                    .collect(),
                self.rooms
                    .iter()
                    .filter(|room| room.kind == RoomKind::Direct && !room.invite)
                    .collect(),
            ),
        };
        let title = space.map_or_else(|| "Home".to_owned(), |space| space.name.clone());
        let parent = space.and_then(|space| self.space_trail.parent(&self.rooms, &space.id));
        let unjoined = space
            .map(|space| {
                space
                    .children
                    .iter()
                    .filter(|id| {
                        *id != &space.id && self.summary(id).is_none_or(|room| room.invite)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let empty = match space {
            Some(_) => rooms.is_empty() && unjoined.is_empty(),
            None => invites.is_empty() && directs.is_empty(),
        };

        let mut list = div()
            .id("channels")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px_2()
            .pb_3()
            .overflow_y_scroll();
        if !invites.is_empty() {
            list = list.child(self.section("INVITES", None, cx)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(invites.into_iter().map(|room| self.render_invite(room, cx))),
            );
        }
        if space.is_some() {
            let (subspaces, rooms): (Vec<_>, Vec<_>) = rooms
                .into_iter()
                .partition(|room| room.kind == RoomKind::Space);
            if !subspaces.is_empty() {
                list = list.child(self.section("SPACES", None, cx)).children(
                    subspaces
                        .into_iter()
                        .map(|space| self.render_subspace(space, cx)),
                );
            }
            if !rooms.is_empty() {
                list = list
                    .child(self.section("ROOMS", Some(Dialog::Join), cx))
                    .children(rooms.into_iter().map(|room| self.render_room_row(room, cx)));
            }
            if !unjoined.is_empty() {
                list = list
                    .child(
                        self.section("NOT JOINED", None, cx).child(
                            Button::new("reload-linked-room-names")
                                .ghost()
                                .small()
                                .p_0()
                                .size(px(22.))
                                .icon("icons/refresh-cw.svg")
                                .tooltip("Reload names")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(space) = &this.space {
                                        this.linked_name_requests.remove(space);
                                    }
                                    this.load_linked_names();
                                    cx.notify();
                                })),
                        ),
                    )
                    .children(unjoined.into_iter().map(|id| {
                        let address = id.to_string();
                        let name = spaces::linked_name(&self.rooms, &self.linked_names, &id);
                        Button::new(SharedString::from(format!("join-space-child-{id}")))
                            .ghost()
                            .small()
                            .w_full()
                            .label(name.clone())
                            .icon("icons/plus.svg")
                            .tooltip(name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.command(Command::Join(address.clone()));
                                this.notify("Joining…", false, cx);
                            }))
                    }));
            }
        }
        if space.is_none() {
            list = list
                .child(self.section("DIRECT MESSAGES", Some(Dialog::Talk), cx))
                .children(
                    directs
                        .into_iter()
                        .map(|room| self.render_room_row(room, cx)),
                );
        }
        if empty {
            list = list.child(
                div()
                    .mt_4()
                    .px_2()
                    .text_size(theme.text(Text::Small))
                    .text_color(theme.muted_foreground)
                    .child(match space {
                        Some(_) => "There are no rooms or spaces here yet.",
                        None => "Nothing here yet. Use the + buttons to join a room or message a friend.",
                    }),
            );
        }

        let direction = if self.space_backwards { -1. } else { 1. };
        let transition_id =
            SharedString::from(format!("space-navigation-{}", self.space_transition));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w(px(0.))
            .h_full()
            .bg(theme.secondary)
            .border_r_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(48.))
                            .px_4()
                            .border_b_1()
                            .border_color(theme.border)
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .gap_2()
                            .when_some(parent, |header, parent| {
                                header.child(
                                    Button::new("space-back")
                                        .ghost()
                                        .small()
                                        .p_0()
                                        .size(px(28.))
                                        .icon("icons/chevron-left.svg")
                                        .tooltip(format!("Back to {}", parent.name))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(current) = &this.space
                                                && let Some(parent) =
                                                    this.space_trail.back(&this.rooms, current)
                                            {
                                                this.animate_space_navigation(true);
                                                this.space = Some(parent);
                                                this.load_linked_names();
                                                cx.notify();
                                            }
                                        })),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(title)),
                    )
                    .when_some(space, |sidebar, space| {
                        let id = space.id.clone();
                        let settings_id = id.clone();
                        sidebar.child(
                            div()
                                .px_2()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .when(space.can_manage, |body| {
                                    body.child(
                                        Button::new("space-settings")
                                            .ghost()
                                            .small()
                                            .w_full()
                                            .icon("icons/settings.svg")
                                            .label("Space settings")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_room_settings_for(
                                                    settings_id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("remove-space")
                                        .ghost()
                                        .small()
                                        .w_full()
                                        .icon("icons/log-out.svg")
                                        .label("Remove space")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.remove_space(id.clone(), window, cx)
                                        })),
                                ),
                        )
                    })
                    .child(list)
                    .motion(
                        transition_id,
                        ui::motion::Motion::Base,
                        move |panel, amount| {
                            let amount = amount.clamp(0., 1.);
                            panel
                                .opacity(amount)
                                .layer_translate(point(px(direction * 18. * (1. - amount)), px(0.)))
                        },
                    ),
            )
            .child(self.render_user_panel(cx))
    }

    fn render_user_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let (id, name, picture) = self
            .me
            .as_ref()
            .map(|me| (me.id.to_string(), me.name.clone(), me.avatar.clone()))
            .unwrap_or_default();
        let verified = self.security.is_none_or(|(verified, _)| verified);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(56.))
            .px_2()
            .bg(theme.sidebar)
            .child(
                div()
                    .id("open-profile")
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .h(px(40.))
                    .px_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary_hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_dialog(Dialog::Profile, window, cx)
                    }))
                    .child(
                        div()
                            .relative()
                            .child(self.face(&id, &name, picture.as_ref(), px(34.)))
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(-1.))
                                    .right(px(-1.))
                                    .size(px(12.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(theme.sidebar)
                                    .bg(green()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(theme.foreground)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(theme.text(Text::Small))
                                    .child(name),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .text_size(px(11.))
                                    .child(id),
                            ),
                    ),
            )
            .child(
                Button::new("open-security")
                    .ghost()
                    .small()
                    .icon(if verified {
                        "icons/shield-check.svg"
                    } else {
                        "icons/shield-alert.svg"
                    })
                    .when(!verified, |button| button.tint(amber()))
                    .tooltip("huddle-security")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_dialog(Dialog::Security, window, cx)
                    })),
            )
            .child(
                Button::new("open-settings")
                    .ghost()
                    .small()
                    .icon("icons/settings.svg")
                    .tooltip("huddle-settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings))),
            )
            .child(
                Button::new("sign-out")
                    .ghost()
                    .small()
                    .icon("icons/log-out.svg")
                    .tooltip("huddle-sign-out")
                    .on_click(cx.listener(|this, _, _, _| {
                        this.stop_typing();
                        this.command(Command::SignOut);
                    })),
            )
    }

    fn measure_main_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Pixels {
        let theme = *cx.theme();
        let width = self.list.viewport_bounds().size.width;
        div()
            .flex()
            .flex_col()
            .w_full()
            .text_size(theme.text(Text::Body))
            .text_color(theme.foreground)
            .child(self.render_main_item(index, cx))
            .into_any_element()
            .layout_as_root(
                gpui::size(
                    gpui::AvailableSpace::Definite(width),
                    gpui::AvailableSpace::MinContent,
                ),
                window,
                cx,
            )
            .height
    }

    fn restore_history_anchor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((room, anchor, old_height)) = self.history_anchor.take() else {
            return;
        };
        if self.room.as_ref() != Some(&room) || anchor.item_ix >= self.list.item_count() {
            return;
        }
        // The old first row loses its history header and can change date/sender grouping.
        // Anchor its unchanged message content, rather than the top of that mutable row.
        let anchor = rebase_history_anchor(anchor, old_height, |index| {
            self.measure_main_item(index, window, cx)
        });
        self.list.scroll_to(anchor);
    }

    fn render_main_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(timeline) = self.timeline() else {
            return div().into_any_element();
        };
        let messages = &timeline.messages;
        let Some(message) = messages.get(index) else {
            return div().into_any_element();
        };
        let previous = index.checked_sub(1).and_then(|index| messages.get(index));
        let entry = self.render_entry(message, previous, Spot::Main, cx);
        let last = index + 1 == messages.len();
        div()
            .flex()
            .flex_col()
            .w_full()
            .when(index == 0, |item| item.child(self.render_start(cx)))
            .child(entry)
            .when(last, |item| item.pb_4())
            .into_any_element()
    }

    fn render_start(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let Some(timeline) = self.timeline() else {
            return div().into_any_element();
        };
        let room = self.room.as_ref().and_then(|room| self.summary(room));
        let direct = room.is_some_and(|room| room.kind == RoomKind::Direct);
        let name = room.map_or_else(String::new, |room| room.name.clone());
        if timeline.loading || timeline.token.is_some() {
            return div()
                .flex()
                .justify_center()
                .py_3()
                .child(
                    Button::new("load-older")
                        .ghost()
                        .small()
                        .label(if timeline.loading {
                            "Loading older messages…"
                        } else {
                            "Load older messages"
                        })
                        .disabled(timeline.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.older(cx))),
                )
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pt_8()
            .pb_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(64.))
                    .rounded_full()
                    .bg(theme.secondary)
                    .text_size(px(32.))
                    .text_color(theme.foreground)
                    .child(if direct { "@" } else { "#" }),
            )
            .child(
                div()
                    .text_size(theme.text(Text::Title))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child(match direct {
                        true => name.clone(),
                        false => format!("Welcome to #{name}!"),
                    }),
            )
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(match direct {
                        true => format!("This is the beginning of your conversation with {name}."),
                        false => "This is the start of the room.".to_owned(),
                    }),
            )
            .into_any_element()
    }

    fn render_thread_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let (Some(timeline), Some(root)) = (self.timeline(), self.thread.as_ref()) else {
            return div().into_any_element();
        };
        if index == 0 {
            let Some(message) = timeline.find(root) else {
                return div()
                    .p_4()
                    .text_color(theme.muted_foreground)
                    .child("The first message of this thread isn't loaded.")
                    .into_any_element();
            };
            let count = timeline.threads.get(root).map_or(0, Vec::len);
            return div()
                .flex()
                .flex_col()
                .pt_2()
                .child(self.render_entry(message, None, Spot::Thread, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .mt_3()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(match count {
                            1 => "1 reply".to_owned(),
                            count => format!("{count} replies"),
                        })
                        .child(div().flex_1().h(px(1.)).bg(theme.border)),
                )
                .into_any_element();
        }
        let Some(replies) = timeline.threads.get(root) else {
            return div().into_any_element();
        };
        let Some(message) = replies.get(index - 1) else {
            return div().into_any_element();
        };
        let previous = index.checked_sub(2).and_then(|index| replies.get(index));
        let last = index == replies.len();
        div()
            .flex()
            .flex_col()
            .child(self.render_entry(message, previous, Spot::Thread, cx))
            .when(last, |item| item.pb_4())
            .into_any_element()
    }

    fn render_entry(
        &self,
        message: &Message,
        previous: Option<&Message>,
        spot: Spot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let key = format!(
            "{}-{}",
            if spot == Spot::Main { "m" } else { "t" },
            message_key(message)
        );
        let new_day =
            previous.is_none_or(|previous| day(previous.timestamp) != day(message.timestamp));
        let grouped = !new_day
            && message.reply.is_none()
            && previous.is_some_and(|previous| {
                previous.sender == message.sender
                    && message.timestamp - previous.timestamp < GROUP_GAP
                    && previous.kind != MessageKind::Emote
                    && message.kind != MessageKind::Emote
            });
        let pending = message.id.is_none();
        let mine = self.me.as_ref().is_some_and(|me| me.id == message.sender);
        let on_link = self.on_link(cx);
        let on_select = self.on_select(cx);
        let editing = matches!(&self.editing, Some((id, at)) if Some(id) == message.id.as_ref() && *at == spot);
        let person = self.person_of(
            &message.sender,
            &message.sender_name,
            message.avatar.as_ref(),
        );

        let rich = |faded: bool, cx: &App| {
            rich::render(
                &key,
                &message.rich,
                faded,
                &on_link,
                &self.selection,
                &on_select,
                cx,
            )
        };
        let body = match message.kind {
            MessageKind::Text => rich(pending, cx),
            MessageKind::Notice => rich(true, cx),
            MessageKind::Emote => div()
                .flex()
                .gap_1()
                .italic()
                .child(
                    self.person_hit(
                        SharedString::from(format!("{key}-emote")),
                        person.clone(),
                        cx,
                    )
                    .flex_none()
                    .hover(|style| style.underline())
                    .child(format!("* {}", message.sender_name)),
                )
                .child(rich(pending, cx))
                .into_any_element(),
            MessageKind::Image => div()
                .flex()
                .flex_col()
                .gap_1()
                .when_some(message.picture.as_ref(), |body, picture| {
                    body.child(self.render_picture(picture, cx))
                })
                .when(!message.rich.is_empty(), |body| body.child(rich(false, cx)))
                .into_any_element(),
            MessageKind::Attachment
                if message.picture.as_ref().is_some_and(|p| {
                    p.mime.as_deref().is_some_and(|m| m.starts_with("video/"))
                }) =>
            {
                let picture = message.picture.as_ref().unwrap();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.render_video(picture, cx))
                    .when(!message.rich.is_empty(), |body| body.child(rich(false, cx)))
                    .into_any_element()
            }
            MessageKind::Attachment
                if message.picture.as_ref().is_some_and(|media| {
                    media
                        .mime
                        .as_deref()
                        .is_some_and(|mime| mime.starts_with("audio/"))
                }) =>
            {
                self.render_audio(message.picture.as_ref().unwrap(), &key, spot, cx)
            }
            MessageKind::Attachment => match &message.picture {
                Some(file) => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.render_file_download(file, &key, cx))
                    .when(!message.rich.is_empty(), |body| body.child(rich(false, cx)))
                    .into_any_element(),
                None => div()
                    .text_color(theme.muted_foreground)
                    .child(message.body.clone())
                    .into_any_element(),
            },
            MessageKind::Encrypted => {
                let verified = self.security.is_none_or(|(verified, _)| verified);
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .italic()
                    .text_color(theme.muted_foreground)
                    .child(icon("icons/lock.svg", px(14.), theme.muted_foreground))
                    .child(match verified {
                        true => "Encrypted message. The key hasn't arrived yet.",
                        false => "Encrypted message. Verify this device to read it.",
                    })
                    .when(!verified, |row| {
                        row.child(
                            div()
                                .id(SharedString::from(format!("{key}-verify")))
                                .not_italic()
                                .text_color(theme.primary)
                                .cursor_pointer()
                                .hover(|style| style.underline())
                                .child("Verify")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_dialog(Dialog::Security, window, cx)
                                })),
                        )
                    })
                    .into_any_element()
            }
        };

        let reply = message
            .reply
            .as_ref()
            .map(|target| self.render_reply_preview(&key, target, cx));

        let reactions = (!message.reactions.is_empty()).then(|| {
            let me = self.my_id();
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .mt_1()
                .children(message.reactions.iter().map(|reaction| {
                    let mine = me
                        .as_ref()
                        .is_some_and(|me| reaction.senders.iter().any(|(sender, _)| sender == me));
                    let target = message.clone();
                    let emoji = reaction.key.clone();
                    div()
                        .id(SharedString::from(format!(
                            "{key}-reaction-{}",
                            reaction.key
                        )))
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(24.))
                        .px_2()
                        .rounded(px(8.))
                        .border_1()
                        .cursor_pointer()
                        .when_else(
                            mine,
                            |chip| {
                                chip.bg(theme.primary.opacity(0.15))
                                    .border_color(theme.primary)
                            },
                            |chip| {
                                chip.bg(theme.secondary)
                                    .border_color(theme.secondary)
                                    .hover(|style| style.border_color(theme.border))
                            },
                        )
                        .child(reaction.key.clone())
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if mine {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                })
                                .child(reaction.senders.len().to_string()),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.react(&target, &emoji, cx)),
                        )
                }))
        });

        let thread_summary = (spot == Spot::Main && message.replies > 0)
            .then(|| message.id.clone())
            .flatten()
            .map(|root| {
                div()
                    .id(SharedString::from(format!("{key}-thread")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_1()
                    .px_2()
                    .py_1()
                    .w_auto()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .cursor_pointer()
                    .text_size(theme.text(Text::Small))
                    .text_color(theme.primary)
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|style| style.bg(theme.secondary_hover))
                    .child(icon("icons/message-square.svg", px(14.), theme.primary))
                    .child(match message.replies {
                        1 => "1 reply".to_owned(),
                        count => format!("{count} replies"),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_thread(root.clone(), window, cx)
                    }))
            });

        let edited = message.edited.then(|| {
            div()
                .text_size(px(10.))
                .text_color(theme.muted_foreground)
                .child("(edited)")
        });

        let content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .when(!grouped, |content| {
                content.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            self.person_hit(
                                SharedString::from(format!("{key}-name")),
                                person.clone(),
                                cx,
                            )
                            .flex_none()
                            .hover(|style| style.underline())
                            .text_color(theme.foreground)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(message.sender_name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted_foreground)
                                .child(match pending {
                                    true => "Sending…".to_owned(),
                                    false => stamp(message.timestamp),
                                }),
                        ),
                )
            })
            .child(body)
            .children(edited)
            .children(reactions)
            .children(thread_summary)
            .children(self.receipt_line(message, spot, cx));

        let toolbar = (!pending && message.kind != MessageKind::Encrypted)
            .then(|| self.render_toolbar(message, &key, spot, mine, cx));

        let row = div()
            .group("message")
            .relative()
            .flex()
            .gap_4()
            .w_full()
            .px_4()
            .when_else(
                editing,
                |row| row.bg(theme.primary.opacity(0.08)),
                |row| row.hover(|style| style.bg(theme.secondary.opacity(0.6))),
            )
            .when_else(
                grouped,
                |row| {
                    row.py(px(2.)).child(
                        div()
                            .flex()
                            .justify_end()
                            .w(AVATAR)
                            .flex_none()
                            .pt(px(3.))
                            .text_size(px(10.))
                            .text_color(theme.muted_foreground.opacity(0.))
                            .group_hover("message", |style| {
                                style.text_color(theme.muted_foreground)
                            })
                            .child(clock(message.timestamp)),
                    )
                },
                |row| {
                    row.mt_3().pt_1().pb(px(2.)).child(
                        self.person_hit(
                            SharedString::from(format!("{key}-face")),
                            person.clone(),
                            cx,
                        )
                        .flex_none()
                        .rounded_full()
                        .child(self.face(
                            message.sender.as_str(),
                            &message.sender_name,
                            message.avatar.as_ref(),
                            AVATAR,
                        )),
                    )
                },
            )
            .child(content)
            .children(toolbar);

        div()
            .flex()
            .flex_col()
            .w_full()
            .when(new_day && spot == Spot::Main, |item| {
                item.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .mt_4()
                        .child(div().flex_1().h(px(1.)).bg(theme.border))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child(day_label(message.timestamp)),
                        )
                        .child(div().flex_1().h(px(1.)).bg(theme.border)),
                )
            })
            .children(reply)
            .child(row)
            .into_any_element()
    }

    fn render_reply_preview(
        &self,
        key: &str,
        target: &OwnedEventId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let original = self.timeline().and_then(|timeline| timeline.find(target));
        let index = self
            .timeline()
            .and_then(|timeline| match timeline.locate(target) {
                Some(Place::Main(index)) => Some(index),
                _ => None,
            });
        let row = div()
            .id(SharedString::from(format!("{key}-reply")))
            .flex()
            .items_center()
            .gap_1()
            .mt_3()
            .pl(px(36.))
            .pr_4()
            .text_size(theme.text(Text::Small))
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .w(px(28.))
                    .h(px(10.))
                    .mt(px(6.))
                    .mr_1()
                    .flex_none()
                    .border_l_2()
                    .border_t_2()
                    .rounded_tl(px(6.))
                    .border_color(theme.border),
            );
        let row = match original {
            Some(original) => row
                .cursor_pointer()
                .hover(|style| style.text_color(theme.foreground))
                .child(self.face(
                    original.sender.as_str(),
                    &original.sender_name,
                    original.avatar.as_ref(),
                    px(16.),
                ))
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hue(original.sender.as_str()))
                        .child(original.sender_name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(match original.kind {
                            MessageKind::Image => "Sent an image".to_owned(),
                            MessageKind::Encrypted => "Encrypted message".to_owned(),
                            _ => rich::preview(&original.rich),
                        }),
                ),
            None => row.italic().child("Replying to an older message"),
        };
        row.on_click(cx.listener(move |this, _, _, _| {
            if let Some(index) = index {
                this.list.scroll_to_reveal_item(index);
            }
        }))
        .into_any_element()
    }

    fn render_video(&self, video: &Picture, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let key = format!("{}#poster", video.key);
        let state = {
            let mut cache = self.media.borrow_mut();
            if let Some(state) = cache.get(&key) {
                state.clone()
            } else {
                self.command(Command::VideoPreview {
                    key: video.key.clone(),
                    source: video.source.clone(),
                    thumbnail: video.thumbnail.clone(),
                });
                cache.insert(key.clone(), Media::Loading);
                Media::Loading
            }
        };
        let (width, height) = if video.width > 0 && video.height > 0 {
            fit(video.width, video.height)
        } else {
            (320., 180.)
        };
        let open = video.clone();
        let loading = self
            .pending_play
            .as_ref()
            .is_some_and(|pending| pending.key == video.key);
        media_ui::card(cx)
            .mt_1()
            .w(px(width))
            .max_w_full()
            .child(
                div()
                    .id(SharedString::from(format!("video-{}", video.key)))
                    .relative()
                    .w_full()
                    .aspect_ratio(width / height)
                    .overflow_hidden()
                    .bg(media_ui::opaque(theme.background))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !loading {
                            this.open_media(open.clone(), cx);
                        }
                    }))
                    .when_some(
                        match state {
                            Media::Ready(image) => Some(image),
                            _ => None,
                        },
                        |body, image| {
                            body.child(img(image).size_full().object_fit(ObjectFit::Contain))
                        },
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(48.))
                                    .rounded_full()
                                    .bg(media_ui::opaque(theme.popover))
                                    .shadow_md()
                                    .child(icon(
                                        "icons/play-filled.svg",
                                        px(20.),
                                        theme.popover_foreground,
                                    )),
                            ),
                    )
                    .when(loading, |body| {
                        body.child(
                            div()
                                .absolute()
                                .bottom_2()
                                .left_2()
                                .right_2()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .bg(media_ui::opaque(theme.popover))
                                .text_size(theme.text(Text::Small))
                                .child("Loading video…"),
                        )
                    }),
            )
            .child(
                media_ui::caption(
                    &video.name,
                    "Video",
                    video.duration.map(playback::time_label).unwrap_or_default(),
                    cx,
                )
                .child(self.media_more(
                    video,
                    SharedString::from(format!("video-more-{}", video.key)),
                    cx,
                )),
            )
            .into_any_element()
    }

    fn render_audio(
        &self,
        media: &Picture,
        key: &str,
        spot: Spot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let player = self
            .player
            .as_ref()
            .filter(|_| self.player_audio.as_ref() == Some(&media.key));
        let state = player.map(|player| player.state.lock().unwrap_or_else(|e| e.into_inner()));
        let loading = self
            .pending_play
            .as_ref()
            .is_some_and(|pending| pending.key == media.key);
        let failed = state.as_ref().is_some_and(|state| state.error.is_some());
        let finished = state.as_ref().is_some_and(|state| state.finished);
        let playing = player.is_some_and(|player| !player.is_paused()) && !finished && !failed;
        let duration = state
            .as_ref()
            .and_then(|state| state.duration)
            .or(media.duration)
            .filter(|duration| !duration.is_zero());
        let position = state.as_ref().map_or(Duration::ZERO, |state| {
            self.player_scrub.unwrap_or(state.position)
        });
        let fraction = duration.map_or(0., |duration| {
            (position.as_secs_f64() / duration.as_secs_f64()).clamp(0., 1.) as f32
        });
        let action = if loading {
            "Loading…"
        } else if failed {
            "Retry"
        } else if finished {
            "Replay"
        } else if playing {
            "Pause"
        } else {
            "Play"
        };
        let open = media.clone();
        let timeline = if spot == Spot::Thread {
            &self.audio_thread_timeline
        } else {
            &self.player_timeline
        };
        let track = if player.is_some() {
            Scrubber::new(timeline, fraction)
                .colors(theme.primary, theme.border, theme.primary)
                .enabled(duration.is_some() && !failed)
                .when_some(self.player_scrub, |bar, time| {
                    bar.bubble(fraction, playback::time_label(time))
                })
                .on_move(cx.listener(move |this, fraction: &f32, _, cx| {
                    if let Some(duration) = duration {
                        this.player_scrub =
                            Some(duration.mul_f64((*fraction as f64).clamp(0., 1.)));
                        cx.notify();
                    }
                }))
                .on_release(cx.listener(|this, _, _, cx| {
                    if let Some(position) = this.player_scrub.take() {
                        if let Some(player) = &this.player {
                            player.seek_to(position);
                        }
                        cx.notify();
                    }
                }))
                .into_any_element()
        } else {
            div()
                .h(px(18.))
                .flex()
                .items_center()
                .child(div().w_full().h(px(3.)).rounded_full().bg(theme.border))
                .into_any_element()
        };
        let status = if loading {
            "Loading…"
        } else if failed {
            "Couldn't play"
        } else if state.as_ref().is_some_and(|state| state.seeking) {
            "Seeking…"
        } else if playing {
            "Playing"
        } else if finished {
            "Finished"
        } else if player.is_some() {
            "Paused"
        } else {
            ""
        };
        media_ui::card(cx)
            .mt_1()
            .w(px(340.))
            .max_w_full()
            .child(
                media_ui::caption(&media.name, "Audio", status.into(), cx)
                    .border_t_0()
                    .child(self.media_more(
                        media,
                        SharedString::from(format!("{key}-audio-more")),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .pb_3()
                    .child(
                        media_ui::play(
                            SharedString::from(format!("{key}-audio-play")),
                            if playing {
                                "icons/pause-filled.svg"
                            } else if finished {
                                "icons/rotate-ccw-clock.svg"
                            } else {
                                "icons/play-filled.svg"
                            },
                            action,
                        )
                        .disabled(loading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !failed
                                && this.player_audio.as_ref() == Some(&open.key)
                                && let Some(player) = &this.player
                            {
                                if finished {
                                    player.restart();
                                } else {
                                    player.toggle();
                                }
                            } else {
                                this.open_media(open.clone(), cx);
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(track)
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_size(px(11.))
                                    .text_color(theme.muted_foreground)
                                    .child(playback::time_label(position))
                                    .child(
                                        duration
                                            .map(playback::time_label)
                                            .unwrap_or_else(|| "—".into()),
                                    ),
                            ),
                    ),
            )
            .when_some(
                state.as_ref().and_then(|state| state.error.clone()),
                |body, error| body.child(div().px_3().pb_3().child(media_ui::error(error, cx))),
            )
            .into_any_element()
    }

    fn open_media(&mut self, media: Picture, cx: &mut Context<Self>) {
        self.media_zoom.reset();
        self.viewing = None;
        self.player = None;
        self.player_audio = None;
        self.player_image = None;
        self.player_scrub = None;
        self.pending_play = None;
        if let Some(bytes) = self.files.get(&format!("{}#original", media.key)).cloned() {
            self.start_media(media, bytes.to_vec(), cx);
        } else {
            self.pending_play = Some(media.clone());
            self.command(Command::Fetch {
                key: media.key,
                source: media.source,
                size: None,
            });
            self.notify("Loading media…", false, cx);
        }
    }

    fn start_media(&mut self, media: Picture, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.player_media = Some(media.clone());
        self.player_image = None;
        self.player_scrub = None;
        self.player_audio = media
            .mime
            .as_deref()
            .is_some_and(|mime| mime.starts_with("audio/"))
            .then(|| media.key.clone());
        self.player = Some(playback::Player::new(
            media.name,
            bytes,
            media.mime.unwrap_or_default(),
        ));
        let state = self.player.as_ref().unwrap().state.clone();
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.duration = media.duration.or(state.duration);
        }
        cx.spawn(async move |this, cx| {
            let mut last_status = None;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let active = this
                    .update(cx, |this, cx| {
                        if !this
                            .player
                            .as_ref()
                            .is_some_and(|player| Arc::ptr_eq(&player.state, &state))
                        {
                            return false;
                        }
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        let has_frame = state.frame.is_some();
                        if let Some(image) = state.frame.take() {
                            this.player_image = Some(image);
                        }
                        let status = (
                            state.finished,
                            state.seeking,
                            state.position.as_millis() / 100,
                            state.duration,
                            state.error.clone(),
                        );
                        if has_frame || last_status.as_ref() != Some(&status) {
                            cx.notify();
                        }
                        last_status = Some(status);
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn close_media(&mut self, cx: &mut Context<Self>) {
        self.player_media = None;
        self.player = None;
        self.player_audio = None;
        self.player_image = None;
        self.player_scrub = None;
        self.pending_play = None;
        cx.notify();
    }

    fn render_zoom_controls(&self, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let factor = self.media_zoom.factor;
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("media-zoom-out")
                    .ghost()
                    .size(px(32.))
                    .p_0()
                    .label("−")
                    .tooltip("Zoom out")
                    .disabled(!enabled || factor <= 1.)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let center = this.media_zoom.center();
                        this.media_zoom.at(this.media_zoom.factor / 1.25, center);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("media-zoom-reset")
                    .ghost()
                    .small()
                    .label(format!("{:.0}%", factor * 100.))
                    .tooltip("Fit to window")
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.media_zoom.reset();
                        cx.notify();
                    })),
            )
            .child(
                media_ui::tool("media-zoom-in", "icons/plus.svg", "Zoom in")
                    .disabled(!enabled || factor >= media_zoom::MAX)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let center = this.media_zoom.center();
                        this.media_zoom.at(this.media_zoom.factor * 1.25, center);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_zoom_stage(
        &self,
        content: AnyElement,
        height: Pixels,
        aspect: f32,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let factor = if enabled { self.media_zoom.factor } else { 1. };
        let offset = if enabled {
            self.media_zoom.offset()
        } else {
            gpui::Point::default()
        };
        let geometry = self.media_zoom.geometry.clone();
        div()
            .id("media-zoom-stage")
            .relative()
            .w_full()
            .h(height)
            .flex_none()
            .overflow_hidden()
            .bg(media_ui::opaque(theme.background))
            .when(factor > 1., |stage| {
                stage.cursor(if self.media_zoom.dragging() {
                    gpui::CursorStyle::ClosedHand
                } else {
                    gpui::CursorStyle::OpenHand
                })
            })
            .on_scroll_wheel(
                cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                    cx.stop_propagation();
                    if enabled {
                        let delta = event
                            .delta
                            .pixel_delta(px(24.))
                            .y
                            .as_f32()
                            .clamp(-120., 120.);
                        this.media_zoom.at(
                            this.media_zoom.factor * (delta * 0.006).exp(),
                            event.position,
                        );
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if enabled {
                        if event.click_count == 2 {
                            let factor = if this.media_zoom.factor > 1. { 1. } else { 2. };
                            this.media_zoom.at(factor, event.position);
                        } else {
                            this.media_zoom.begin_drag(event.position);
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.dragging() && this.media_zoom.drag_to(event.position) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.media_zoom.end_drag() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.media_zoom.end_drag() {
                        cx.notify();
                    }
                }),
            )
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        let old = geometry.get();
                        geometry.set(media_zoom::Geometry { bounds, aspect });
                        if old.bounds != bounds || old.aspect != aspect {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .w(gpui::relative(factor))
                    .h(gpui::relative(factor))
                    .left(gpui::relative((1. - factor) / 2.))
                    .top(gpui::relative((1. - factor) / 2.))
                    .ml(offset.x)
                    .mt(offset.y)
                    .child(content),
            )
            .into_any_element()
    }

    fn render_player(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.player_audio.is_some() {
            return div().into_any_element();
        }
        let Some(player) = &self.player else {
            return div().into_any_element();
        };
        let theme = *cx.theme();
        let state = player
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let failed = state.error.is_some();
        let finished = state.finished;
        let paused = player.is_paused();
        let status = if failed {
            "Couldn't play"
        } else if state.seeking {
            "Seeking…"
        } else if finished {
            "Finished"
        } else if paused {
            "Paused"
        } else if self.player_image.is_none() {
            "Loading video…"
        } else {
            "Playing"
        };
        let viewport = window.viewport_size();
        let position = self.player_scrub.unwrap_or(state.position);
        let duration = state.duration.filter(|duration| !duration.is_zero());
        let fraction = duration.map_or(0., |duration| {
            (position.as_secs_f64() / duration.as_secs_f64()).clamp(0., 1.) as f32
        });
        let stage_height = (viewport.height - px(240.)).max(px(80.)).min(px(600.));
        let panel_width = (viewport.width - px(48.)).max(px(0.)).min(px(960.));
        let close = media_ui::tool("media-close", "icons/x.svg", "Close (Esc)")
            .on_click(cx.listener(|this, _, _, cx| this.close_media(cx)));
        let content = div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(media_ui::opaque(theme.background))
            .when_else(
                self.player_image.is_some(),
                |stage| {
                    stage.child(
                        img(self.player_image.clone().unwrap())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                },
                |stage| {
                    stage.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.muted_foreground)
                            .child(icon("icons/video.svg", px(32.), theme.muted_foreground))
                            .child(status),
                    )
                },
            );
        let aspect = self.player_image.as_ref().map_or(0., |image| {
            let size = image.size(0);
            if size.height.0 > 0 {
                size.width.0 as f32 / size.height.0 as f32
            } else {
                0.
            }
        });
        let stage = self.render_zoom_stage(
            content.into_any_element(),
            stage_height,
            aspect,
            self.player_image.is_some(),
            cx,
        );
        let timeline = Scrubber::new(&self.player_timeline, fraction)
            .colors(theme.primary, theme.border, theme.primary)
            .enabled(duration.is_some() && !failed)
            .when_some(self.player_scrub, |bar, time| {
                bar.bubble(fraction, playback::time_label(time))
            })
            .on_move(cx.listener(move |this, fraction: &f32, _, cx| {
                if let Some(duration) = duration {
                    this.player_scrub = Some(duration.mul_f64((*fraction as f64).clamp(0., 1.)));
                    cx.notify();
                }
            }))
            .on_release(cx.listener(|this, _, _, cx| {
                if let Some(position) = this.player_scrub.take() {
                    if let Some(player) = &this.player {
                        player.seek_to(position);
                    }
                    cx.notify();
                }
            }));
        let play = media_ui::play(
            "media-toggle",
            if finished {
                "icons/rotate-ccw-clock.svg"
            } else if paused {
                "icons/play-filled.svg"
            } else {
                "icons/pause-filled.svg"
            },
            if finished {
                "Replay"
            } else if paused {
                "Play"
            } else {
                "Pause"
            },
        )
        .disabled(failed)
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(player) = &this.player {
                if finished {
                    player.restart();
                } else {
                    player.toggle();
                }
            }
            cx.notify();
        }));
        let muted = player.is_muted();
        let volume = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                media_ui::tool(
                    "media-mute",
                    if muted {
                        "icons/volume-x.svg"
                    } else {
                        "icons/volume-2.svg"
                    },
                    if muted { "Unmute" } else { "Mute" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(player) = &this.player {
                        player.mute();
                    }
                    cx.notify();
                })),
            )
            .child(
                div().w(px(88.)).child(
                    Scrubber::new(&self.player_volume, player.volume() as f32 / 100.)
                        .colors(theme.primary, theme.border, theme.primary)
                        .on_move(cx.listener(|this, fraction: &f32, _, cx| {
                            if let Some(player) = &this.player {
                                let target = (fraction.clamp(0., 1.) * 100.).round() as i32;
                                player.change_volume(target - player.volume() as i32);
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .w(px(32.))
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(format!("{}%", player.volume())),
            );
        let controls = div()
            .px_4()
            .pt_2()
            .pb_4()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .child(timeline)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("media-back")
                                    .ghost()
                                    .small()
                                    .label("−10s")
                                    .tooltip("Back 10 seconds")
                                    .disabled(failed)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(player) = &this.player {
                                            player.skip(-10);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(play)
                            .child(
                                Button::new("media-forward")
                                    .ghost()
                                    .small()
                                    .label("+10s")
                                    .tooltip("Forward 10 seconds")
                                    .disabled(failed)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(player) = &this.player {
                                            player.skip(10);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(80.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(theme.text(Text::Small)).child(format!(
                                    "{} / {}",
                                    playback::time_label(position),
                                    duration
                                        .map(playback::time_label)
                                        .unwrap_or_else(|| "—".into())
                                )))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted_foreground)
                                    .child(status),
                            ),
                    )
                    .child(volume),
            )
            .when_some(state.error.clone(), |body, error| {
                body.child(media_ui::error(error, cx))
            });
        let panel = media_ui::card(cx)
            .id("video-viewer-panel")
            .w(panel_width)
            .max_h((viewport.height - px(32.)).max(px(0.)))
            .overflow_y_scroll()
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(media_ui::header(
                &player.name,
                "Video",
                vec![
                    self.render_zoom_controls(self.player_image.is_some(), cx),
                    self.player_media
                        .as_ref()
                        .map(|media| {
                            self.media_more(media, "video-file-options".into(), cx)
                                .into_any_element()
                        })
                        .unwrap_or_else(|| div().into_any_element()),
                    close.into_any_element(),
                ],
                cx,
            ))
            .child(stage)
            .child(controls);
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(media_ui::opaque(theme.overlay).opacity(0.8))
                    .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.close_media(cx)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_media(cx);
                        }),
                    )
                    .child(panel)
                    .fading("video-inspect"),
            ),
        )
        .into_any_element()
    }

    fn open_picture(&mut self, picture: Picture, cx: &mut Context<Self>) {
        self.media_zoom.reset();
        if self.player_audio.is_none() {
            self.close_media(cx);
        }
        self.viewing = Some(View {
            name: picture.name.clone(),
            picture,
        });
        cx.notify();
    }

    fn close_picture(&mut self, cx: &mut Context<Self>) {
        self.viewing = None;
        cx.notify();
    }

    fn render_picture(&self, picture: &Picture, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let (width, height) = fit(picture.width, picture.height);
        let size = match &picture.source {
            MediaSource::Plain(_) => Some((800, 600, false)),
            MediaSource::Encrypted(_) => None,
        };
        let open = picture.clone();
        let mut frame = div()
            .id(SharedString::from(format!("picture-{}", picture.key)))
            .flex()
            .items_center()
            .justify_center()
            .w_full()
            .aspect_ratio(width / height)
            .overflow_hidden()
            .bg(media_ui::opaque(theme.background))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.open_picture(open.clone(), cx)));
        frame = match self.want(&picture.key, picture.source.clone(), size) {
            Media::Ready(image) => {
                frame.child(img(image).size_full().object_fit(ObjectFit::Contain))
            }
            Media::Loading => frame.child(icon("icons/image.svg", px(28.), theme.muted_foreground)),
            Media::Failed => frame
                .flex_col()
                .gap_2()
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(icon("icons/image.svg", px(28.), theme.muted_foreground))
                .child("This image couldn't load."),
        };
        let detail = if picture.width > 0 && picture.height > 0 {
            format!("{} × {}", picture.width, picture.height)
        } else {
            String::new()
        };
        media_ui::card(cx)
            .mt_1()
            .w(px(width))
            .max_w_full()
            .child(frame)
            .child(
                media_ui::caption(&picture.name, "Image", detail, cx).child(self.media_more(
                    picture,
                    SharedString::from(format!("image-more-{}", picture.key)),
                    cx,
                )),
            )
            .into_any_element()
    }

    fn render_viewer(
        &self,
        view: &View,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = *cx.theme();
        let viewport = window.viewport_size();
        let local = matches!(&view.picture.source, MediaSource::Plain(uri) if uri.as_str().starts_with("mxc://huddle/"));
        let original = format!("{}#original", view.picture.key);
        let full = if local {
            None
        } else {
            Some(self.want(&original, view.picture.source.clone(), None))
        };
        let thumb_size = match &view.picture.source {
            MediaSource::Plain(_) if !local => Some((1600, 1200, false)),
            _ => None,
        };
        let thumb = self.want(&view.picture.key, view.picture.source.clone(), thumb_size);
        let image = match &full {
            Some(Media::Ready(image)) => Some(image.clone()),
            _ => match thumb {
                Media::Ready(image) => Some(image),
                _ => None,
            },
        };
        let failed = matches!(full, Some(Media::Failed)) && !matches!(image, Some(_));
        let panel_width = (viewport.width - px(48.)).max(px(0.)).min(px(960.));
        let stage_height = (viewport.height - px(144.)).max(px(80.)).min(px(640.));
        let content = div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(media_ui::opaque(theme.background))
            .when_else(
                image.is_some(),
                |stage| {
                    stage.child(
                        img(image.clone().unwrap())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                },
                |stage| {
                    stage.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .text_color(theme.muted_foreground)
                            .child(icon("icons/image.svg", px(32.), theme.muted_foreground))
                            .child(if failed {
                                "This image couldn't load."
                            } else {
                                "Loading image…"
                            }),
                    )
                },
            );
        let aspect = if view.picture.height > 0 {
            view.picture.width as f32 / view.picture.height as f32
        } else {
            0.
        };
        let stage = self.render_zoom_stage(
            content.into_any_element(),
            stage_height,
            aspect,
            image.is_some(),
            cx,
        );
        let actions = vec![
            self.render_zoom_controls(image.is_some(), cx),
            self.media_more(&view.picture, "image-file-options".into(), cx)
                .into_any_element(),
            media_ui::tool("image-close", "icons/x.svg", "Close (Esc)")
                .on_click(cx.listener(|this, _, _, cx| this.close_picture(cx)))
                .into_any_element(),
        ];
        let panel = media_ui::card(cx)
            .id("image-viewer-panel")
            .w(panel_width)
            .max_h((viewport.height - px(32.)).max(px(0.)))
            .overflow_y_scroll()
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(media_ui::header(&view.name, "Image", actions, cx))
            .child(stage);
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(media_ui::opaque(theme.overlay).opacity(0.8))
                    .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.close_picture(cx)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_picture(cx);
                        }),
                    )
                    .child(panel)
                    .fading("image-inspect"),
            ),
        )
    }

    fn render_toolbar(
        &self,
        message: &Message,
        key: &str,
        spot: Spot,
        mine: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let picking = matches!(&self.picker, Some((id, at)) if Some(id) == message.id.as_ref() && *at == spot);
        let tool = |name: &str, path: &'static str, tip: &'static str| {
            Button::new(SharedString::from(format!("{key}-{name}")))
                .ghost()
                .small()
                .icon(path)
                .tooltip(tip)
        };
        let target = message.clone();
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(theme.radius)
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(
                tool("react", "icons/smile-plus.svg", "huddle-react").on_click(cx.listener({
                    let id = message.id.clone();
                    move |this, _, _, cx| {
                        this.picker = match &this.picker {
                            Some((open, at)) if Some(open) == id.as_ref() && *at == spot => None,
                            _ => id.clone().map(|id| (id, spot)),
                        };
                        cx.notify();
                    }
                })),
            );
        if let (Some(room), Some(event)) = (self.room.clone(), message.id.clone()) {
            let root = if spot == Spot::Thread {
                self.thread.clone()
            } else {
                None
            };
            bar = bar.child(
                tool("readers", "icons/circle-check.svg", "Read receipts").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.readers_view = Some(receipts::Selection {
                            room: room.clone(),
                            event: event.clone(),
                            root: root.clone(),
                            limit: 50,
                        });
                        cx.notify();
                    },
                )),
            );
        }
        if spot == Spot::Main {
            let reply = target.clone();
            bar = bar.child(tool("reply", "icons/reply.svg", "huddle-reply").on_click(
                cx.listener(move |this, _, window, cx| this.start_reply(&reply, window, cx)),
            ));
            if let Some(root) = message.id.clone() {
                bar = bar.child(
                    tool("thread", "icons/message-square.svg", "huddle-thread").on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.open_thread(root.clone(), window, cx)
                        }),
                    ),
                );
            }
        }
        if matches!(
            message.kind,
            MessageKind::Text | MessageKind::Emote | MessageKind::Notice
        ) {
            let text = message.body.clone();
            bar = bar.child(
                tool("copy", "icons/copy.svg", "huddle-copy").on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        this.notify("Copied", false, cx);
                    },
                )),
            );
        }
        if mine && message.kind == MessageKind::Text {
            let edit = target.clone();
            bar = bar.child(tool("edit", "icons/pencil.svg", "huddle-edit").on_click(
                cx.listener(move |this, _, window, cx| this.start_edit(&edit, spot, window, cx)),
            ));
        }
        if mine {
            let delete = target.clone();
            bar = bar.child(
                tool("delete", "icons/trash-2.svg", "huddle-delete")
                    .on_click(cx.listener(move |this, _, _, cx| this.delete(&delete, cx))),
            );
        }

        let picker = picking.then(|| {
            let emojis = EMOJI.iter().map(|emoji| {
                let target = target.clone();
                div()
                    .id(SharedString::from(format!("{key}-pick-{emoji}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(34.))
                    .rounded(theme.radius)
                    .text_size(px(20.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary_hover))
                    .child(*emoji)
                    .on_click(cx.listener(move |this, _, _, cx| this.react(&target, emoji, cx)))
            });
            deferred(
                anchored().anchor(Anchor::TopLeft).snap_to_window().child(
                    div()
                        .id(SharedString::from(format!("{key}-picker")))
                        .occlude()
                        .mt_1()
                        .grid()
                        .grid_cols(6)
                        .gap_1()
                        .p_2()
                        .rounded(theme.radius * 1.5)
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .children(emojis)
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.picker = None;
                            cx.notify();
                        })),
                ),
            )
            .with_priority(1)
        });

        div()
            .absolute()
            .top(px(-8.))
            .right(px(16.))
            .flex()
            .flex_col()
            .items_end()
            .when_else(
                picking,
                |wrap| wrap.opacity(1.),
                |wrap| {
                    wrap.opacity(0.)
                        .group_hover("message", |style| style.opacity(1.))
                        .hover(|style| style.opacity(1.))
                },
            )
            .child(bar)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(0.))
                    .flex()
                    .justify_end()
                    .children(picker),
            )
            .into_any_element()
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let name = self.me.as_ref().map_or("there", |me| me.name.as_str());
        let banner = self.render_security_banner(cx);
        let step = |id: &'static str,
                    path: &'static str,
                    title: &'static str,
                    detail: &'static str,
                    dialog: Dialog| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_4()
                .p_4()
                .rounded(theme.radius * 1.5)
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|style| style.border_color(theme.primary).bg(theme.secondary_hover))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(44.))
                        .rounded_full()
                        .bg(theme.primary.opacity(0.15))
                        .child(icon(path, px(20.), theme.primary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(theme.foreground)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(theme.text(Text::Small))
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .child(div().flex_1())
                .child(icon(
                    "icons/chevron-right.svg",
                    px(18.),
                    theme.muted_foreground,
                ))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.show_dialog(dialog, window, cx)),
                )
        };
        let suggestions = SUGGESTED.iter().map(|(title, address, detail)| {
            let address = *address;
            div()
                .id(SharedString::from(format!("suggest-{address}")))
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|style| style.bg(theme.secondary_hover))
                .child(avatar(address, title, px(36.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_color(theme.foreground)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(*title),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(theme.text(Text::Small))
                                .text_color(theme.muted_foreground)
                                .child(*detail),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("join-{address}")))
                        .small()
                        .secondary()
                        .label("Join")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.command(Command::Join(address.to_owned()));
                            this.notify("Joining…", false, cx);
                        })),
                )
        });

        div()
            .id("welcome")
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .w_full()
                    .max_w(px(560.))
                    .px_6()
                    .py_10()
                    .child(
                        div()
                            .text_size(theme.text(Text::Display))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(format!("Welcome, {name}!")),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("Pick a room on the left, or start with one of these:"),
                    )
                    .children(banner)
                    .child(step(
                        "welcome-join",
                        "icons/search.svg",
                        "Join a community",
                        "Got a room address like #room:matrix.org? Hop in.",
                        Dialog::Join,
                    ))
                    .child(step(
                        "welcome-talk",
                        "icons/user-round.svg",
                        "Message a friend",
                        "Start a private chat with someone you know.",
                        Dialog::Talk,
                    ))
                    .child(step(
                        "welcome-create",
                        "icons/square-plus.svg",
                        "Create your own room",
                        "A place for your group, team or friends.",
                        Dialog::Create,
                    ))
                    .child(
                        div()
                            .pt_4()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.muted_foreground)
                            .child("POPULAR PLACES TO START"),
                    )
                    .children(suggestions),
            )
            .fading("welcome")
            .into_any_element()
    }

    fn render_security_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = *cx.theme();
        let (verified, _) = self.security?;
        if verified || self.banner_hidden {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(amber().opacity(0.12))
                .border_b_1()
                .border_color(amber().opacity(0.4))
                .text_size(theme.text(Text::Small))
                .text_color(theme.foreground)
                .child(icon("icons/shield-alert.svg", px(16.), amber()))
                .child(div().flex_1().child(
                    "Verify this device to read encrypted messages sent before you signed in.",
                ))
                .child(
                    Button::new("banner-verify")
                        .small()
                        .primary()
                        .label("Verify")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_dialog(Dialog::Security, window, cx)
                        })),
                )
                .child(
                    Button::new("banner-hide")
                        .small()
                        .ghost()
                        .icon("icons/x.svg")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.banner_hidden = true;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn media_drop_zone(&self, spot: Spot, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let color = cx.theme().primary;
        let enabled = self.phase == Phase::SignedIn
            && self
                .room
                .as_ref()
                .and_then(|id| self.summary(id))
                .is_some_and(|room| !room.invite)
            && (spot == Spot::Main || self.thread.is_some())
            && !matches!(&self.editing, Some((_, at)) if *at == spot);
        div()
            .id(if spot == Spot::Main {
                "room-file-drop"
            } else {
                "thread-file-drop"
            })
            .can_drop(move |value, _, _| enabled && value.is::<ExternalPaths>())
            .when(enabled, |zone| {
                zone.drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(color.opacity(0.08)))
            })
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                this.drop_files(spot, paths, window, cx)
            }))
    }

    fn render_composer(&self, spot: Spot, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let input = match spot {
            Spot::Main => self.draft.clone(),
            Spot::Thread => self.thread_draft.clone(),
        };
        let editing = matches!(&self.editing, Some((_, at)) if *at == spot);
        let replying = match spot {
            Spot::Main => self.replying.clone(),
            Spot::Thread => None,
        };
        let banner = |text: AnyElement, id: &'static str, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(32.))
                .px_3()
                .rounded_t(theme.radius * 1.5)
                .bg(theme.secondary_hover)
                .text_size(theme.text(Text::Small))
                .text_color(theme.muted_foreground)
                .child(div().flex_1().min_w_0().truncate().child(text))
                .child(
                    Button::new(id)
                        .ghost()
                        .small()
                        .icon("icons/x.svg")
                        .on_click(cx.listener(move |this, _, _, cx| this.cancel_compose(spot, cx))),
                )
        };
        let top = if editing {
            Some(banner(
                div()
                    .child("Editing message. Press Escape to cancel.")
                    .into_any_element(),
                if spot == Spot::Main {
                    "cancel-edit"
                } else {
                    "cancel-thread-edit"
                },
                cx,
            ))
        } else {
            replying.map(|target| {
                banner(
                    div()
                        .flex()
                        .gap_1()
                        .child("Replying to")
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(target.name),
                        )
                        .into_any_element(),
                    "cancel-reply",
                    cx,
                )
            })
        };
        let attached = top.is_some();
        let preview = (!editing)
            .then(|| self.attachment.as_ref().filter(|(at, _)| *at == spot))
            .flatten()
            .map(|(_, shot)| {
                let remove = if spot == Spot::Main {
                    "remove-upload"
                } else {
                    "remove-thread-upload"
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_2()
                    .when_some(shot.preview.clone(), |row, preview| {
                        row.child(
                            img(preview)
                                .w(px(72.))
                                .h(px(72.))
                                .flex_none()
                                .rounded(theme.radius)
                                .object_fit(ObjectFit::Cover),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.foreground)
                            .child(shot.name.clone()),
                    )
                    .child(
                        Button::new(remove)
                            .ghost()
                            .small()
                            .icon("icons/x.svg")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.attachment = None;
                                this.cancel_compress();
                                cx.notify();
                            })),
                    )
            });
        let compressing = (self.compressing == Some(spot) && preview.is_none()).then(|| {
            let cancel = if spot == Spot::Main {
                "cancel-compress"
            } else {
                "cancel-thread-compress"
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pt_2()
                .child(
                    div()
                        .flex_1()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child("Preparing media…"),
                )
                .child(
                    Button::new(cancel)
                        .ghost()
                        .small()
                        .icon("icons/x.svg")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cancel_compress();
                            cx.notify();
                        })),
                )
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px_4()
            .pb_5()
            .when(spot == Spot::Main, |composer| {
                let label = self
                    .room
                    .as_ref()
                    .and_then(|room| self.typing.label(room, std::time::Instant::now()))
                    .unwrap_or("")
                    .to_owned();
                composer.child(
                    div()
                        .id("typing-status")
                        .h(px(20.))
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
            })
            .children(top)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when_else(
                        attached,
                        |row| row.rounded_b(theme.radius * 1.5),
                        |row| row.rounded(theme.radius * 1.5),
                    )
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .when(!editing, |composer| {
                        composer.drag_over::<ExternalPaths>(move |style, _, _, _| {
                            style.border_color(theme.primary)
                        })
                    })
                    .on_action(
                        cx.listener(move |this, _: &Submit, window, cx| {
                            this.send(spot, window, cx)
                        }),
                    )
                    .on_action(
                        cx.listener(move |this, _: &Dismiss, _, cx| this.cancel_compose(spot, cx)),
                    )
                    .children(compressing)
                    .children(preview)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .py(px(6.))
                            .pl_2()
                            .pr_2()
                            .child(
                                Button::new(if spot == Spot::Main {
                                    "upload-image"
                                } else {
                                    "upload-thread-image"
                                })
                                .ghost()
                                .small()
                                .icon("icons/image.svg")
                                .tooltip("huddle-upload")
                                .disabled(editing)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.pick_image(spot, cx)),
                                ),
                            )
                            .child(input)
                            .child(
                                Button::new(if spot == Spot::Main {
                                    "send-message"
                                } else {
                                    "send-thread"
                                })
                                .primary()
                                .small()
                                .label(if editing { "Save" } else { "Send" })
                                .on_click(cx.listener(
                                    move |this, _, window, cx| this.send(spot, window, cx),
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_jump_to_bottom(&self, spot: Spot, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = match spot {
            Spot::Main => &self.list,
            Spot::Thread => &self.thread_list,
        };
        let remaining = list.max_offset_for_scrollbar().y + list.scroll_px_offset_for_scrollbar().y;
        if list.item_count() == 0 || remaining <= px(48.) {
            return None;
        }
        let id = if spot == Spot::Main {
            "chat-jump-bottom"
        } else {
            "thread-jump-bottom"
        };
        Some(
            ui::perched(
                Button::new(id)
                    .icon("icons/chevron-down.svg")
                    .label("Latest messages")
                    .tooltip("Jump to bottom")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        ui::cancel_middle_scroll(cx);
                        let (list, bar) = match spot {
                            Spot::Main => {
                                this.history_anchor = None;
                                (&this.list, &this.chat_scroll)
                            }
                            Spot::Thread => (&this.thread_list, &this.thread_scroll),
                        };
                        list.scroll_to_end();
                        // An in-flight wheel animation must not pull the list back up.
                        bar.update(cx, |bar, _| bar.rebase());
                        cx.notify();
                    })),
                cx,
            )
            .fading(id)
            .into_any_element(),
        )
    }

    fn render_chat(&self, room: &RoomSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let timeline = self.timelines.get(&room.id);
        let loaded = timeline.is_some_and(|timeline| timeline.loaded);
        let empty = timeline.is_none_or(|timeline| timeline.messages.is_empty());
        let direct = room.kind == RoomKind::Direct;
        let other = direct
            .then(|| self.members.get(&room.id))
            .flatten()
            .and_then(|members| {
                let mine = self.me.as_ref().map(|me| &me.id);
                members
                    .iter()
                    .find(|member| Some(&member.id) != mine)
                    .or_else(|| members.first())
                    .map(|member| self.person_of(&member.id, &member.name, member.avatar.as_ref()))
            });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(48.))
            .px_4()
            .border_b_1()
            .border_color(theme.border)
            .child(match &other {
                Some(person) => self
                    .person_hit("chat-face", person.clone(), cx)
                    .rounded_full()
                    .child(self.face(
                        person.id.as_str(),
                        &person.name,
                        person.avatar.as_ref(),
                        px(24.),
                    ))
                    .into_any_element(),
                None if direct => {
                    self.face(room.id.as_str(), &room.name, room.avatar.as_ref(), px(24.))
                }
                None => div()
                    .text_size(px(22.))
                    .text_color(theme.muted_foreground)
                    .child("#")
                    .into_any_element(),
            })
            .child(match &other {
                Some(person) => self
                    .person_hit("chat-name", person.clone(), cx)
                    .flex_none()
                    .hover(|style| style.underline())
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child(person.name.clone())
                    .into_any_element(),
                None => div()
                    .flex_none()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child(room.name.clone())
                    .into_any_element(),
            })
            .when(room.encrypted, |header| {
                header.child(icon("icons/lock.svg", px(14.), theme.muted_foreground))
            })
            .when_some(room.topic.clone(), |header, topic| {
                header
                    .child(
                        div()
                            .flex_none()
                            .w(px(1.))
                            .h(px(22.))
                            .mx_1()
                            .bg(theme.border),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.muted_foreground)
                            .child(topic.lines().next().unwrap_or_default().to_owned()),
                    )
            })
            .child(div().flex_1())
            .when(room.can_manage, |header| {
                header.child(
                    Button::new("room-settings")
                        .ghost()
                        .small()
                        .icon("icons/settings.svg")
                        .tooltip("Room settings")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_room_settings(window, cx)),
                        ),
                )
            })
            .child({
                let id = room.id.clone();
                let name = room.name.clone();
                let direct = room.kind == RoomKind::Direct;
                Button::new("call-voice")
                    .ghost()
                    .small()
                    .icon("icons/phone.svg")
                    .tooltip("huddle-call")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.line = Some(Line::Live {
                            room: id.clone(),
                            name: name.clone(),
                        });
                        this.command(Command::Call {
                            room: id.clone(),
                            direct,
                            voice: true,
                        });
                        cx.notify();
                    }))
            })
            .child({
                let id = room.id.clone();
                let name = room.name.clone();
                let direct = room.kind == RoomKind::Direct;
                Button::new("call-video")
                    .ghost()
                    .small()
                    .icon("icons/video.svg")
                    .tooltip("huddle-video")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.line = Some(Line::Live {
                            room: id.clone(),
                            name: name.clone(),
                        });
                        this.command(Command::Call {
                            room: id.clone(),
                            direct,
                            voice: false,
                        });
                        cx.notify();
                    }))
            })
            .child(
                Button::new("toggle-members")
                    .ghost()
                    .small()
                    .icon("icons/user-round.svg")
                    .selected(self.show_members && self.thread.is_none())
                    .tooltip("huddle-members")
                    .on_click(cx.listener(|this, _, _, cx| {
                        match this.thread.is_some() {
                            true => {
                                this.thread = None;
                                this.show_members = true;
                            }
                            false => this.show_members = !this.show_members,
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("leave-room")
                    .ghost()
                    .small()
                    .icon("icons/log-out.svg")
                    .tooltip("huddle-leave")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_dialog(Dialog::Leave, window, cx)
                    })),
            );

        let body = if !loaded && empty {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Loading messages…")
                .into_any_element()
        } else if empty {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .justify_end()
                .gap_2()
                .p_6()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(64.))
                        .rounded_full()
                        .bg(theme.secondary)
                        .text_size(px(32.))
                        .text_color(theme.foreground)
                        .child(if direct { "@" } else { "#" }),
                )
                .child(
                    div()
                        .text_size(theme.text(Text::Title))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child(match direct {
                            true => room.name.clone(),
                            false => format!("Welcome to #{}!", room.name),
                        }),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(match direct {
                            true => format!(
                                "This is the beginning of your conversation with {}. Say hi!",
                                room.name
                            ),
                            false => {
                                "This is the start of the room. Say hello to get things going."
                                    .to_owned()
                            }
                        }),
                )
                .into_any_element()
        } else {
            Scroller::listing("chat-messages", &self.chat_scroll)
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, index: usize, _, cx| this.render_main_item(index, cx)),
                    )
                    .size_full(),
                )
                .into_any_element()
        };

        let chat = self
            .media_drop_zone(Spot::Main, cx)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(header)
            .children(
                room.encrypted
                    .then(|| self.render_security_banner(cx))
                    .flatten(),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .children(
                        (!empty)
                            .then(|| self.render_jump_to_bottom(Spot::Main, cx))
                            .flatten(),
                    ),
            )
            .child(self.render_composer(Spot::Main, cx));

        let members_open = self.thread.is_none() && self.show_members;
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(chat)
            .child(slide(
                self.render_members(room, cx).into_any_element(),
                "members",
                members_open,
                MEMBERS,
                false,
            ))
            .when(self.thread.is_some(), |row| {
                row.child(slide(
                    self.render_thread_panel(cx).into_any_element(),
                    "thread",
                    true,
                    THREAD_PANEL,
                    true,
                ))
            })
            .fading(SharedString::from(format!("room-{}", room.id)))
            .into_any_element()
    }

    fn render_thread_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        self.media_drop_zone(Spot::Thread, cx)
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w(px(0.))
            .h_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(
                        "icons/message-square.svg",
                        px(18.),
                        theme.muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("Thread"),
                    )
                    .child(
                        Button::new("close-thread")
                            .ghost()
                            .small()
                            .icon("icons/x.svg")
                            .tooltip("huddle-close-thread")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.thread = None;
                                if matches!(this.editing, Some((_, Spot::Thread))) {
                                    this.editing = None;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Scroller::listing("thread-messages", &self.thread_scroll).child(
                            list(
                                self.thread_list.clone(),
                                cx.processor(|this, index: usize, _, cx| {
                                    this.render_thread_item(index, cx)
                                }),
                            )
                            .size_full(),
                        ),
                    )
                    .children(self.render_jump_to_bottom(Spot::Thread, cx)),
            )
            .child(self.render_composer(Spot::Thread, cx))
    }

    fn render_members(&self, room: &RoomSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let members = self
            .members
            .get(&room.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut group = |title: &str, range: std::ops::RangeInclusive<i64>| {
            let people = members
                .iter()
                .filter(|member| range.contains(&member.power))
                .collect::<Vec<_>>();
            (!people.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .pt_4()
                            .pb_1()
                            .px_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.muted_foreground)
                            .child(format!("{title} — {}", people.len())),
                    )
                    .children(people.into_iter().map(|member| {
                        self.person_hit(
                            SharedString::from(format!("member-{}", member.id)),
                            self.person_of(&member.id, &member.name, member.avatar.as_ref()),
                            cx,
                        )
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(40.))
                        .px_2()
                        .rounded(theme.radius)
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(self.face(
                            member.id.as_str(),
                            &member.name,
                            member.avatar.as_ref(),
                            px(30.),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(member.name.clone()),
                        )
                    }))
            })
        };
        div()
            .id("members")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w(px(0.))
            .h_full()
            .px_2()
            .pb_3()
            .bg(theme.secondary)
            .border_l_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .children(group("ADMINS", 100..=i64::MAX))
            .children(group("MODERATORS", 50..=99))
            .children(group("MEMBERS", i64::MIN..=49))
            .when(members.is_empty(), |panel| {
                panel.child(
                    div()
                        .pt_4()
                        .px_2()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child(format!("{} members", room.members)),
                )
            })
    }

    fn render_security(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let close = cx.listener(|this, _: &(), _, cx| this.close_dialog(cx));
        let (verified, recovery) = self.security.unwrap_or((false, Recovery::Unknown));
        let button = |id: &'static str, label: &'static str| Button::new(id).label(label);
        let option =
            |id: &'static str, path: &'static str, title: &'static str, detail: &'static str| {
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme.primary).bg(theme.secondary))
                    .child(icon(path, px(20.), theme.primary))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(
                                div()
                                    .text_size(theme.text(Text::Small))
                                    .text_color(theme.muted_foreground)
                                    .child(detail),
                            ),
                    )
            };
        let back = button("security-back", "Back")
            .ghost()
            .on_click(cx.listener(|this, _, _, cx| {
                this.guard = Guard::Menu;
                cx.notify();
            }));
        let done = button("security-done", "Done")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| {
                this.dialog = None;
                cx.notify();
            }));

        match self.guard.clone() {
            Guard::Menu => {
                let status = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(if verified { green().opacity(0.12) } else { amber().opacity(0.12) })
                    .text_size(theme.text(Text::Small))
                    .child(icon(
                        if verified { "icons/shield-check.svg" } else { "icons/shield-alert.svg" },
                        px(18.),
                        if verified { green() } else { amber() },
                    ))
                    .child(div().flex_1().min_w_0().child(match verified {
                        true => "This device is verified. Your encrypted messages can be read here.",
                        false => "This device isn't verified yet, so older encrypted messages can't be read.",
                    }));
                let mut modal = Modal::new("dialog-security", "Encryption").w(px(440.))
                    .close_button()
                    .on_dismiss(close)
                    .child(status);
                if !verified {
                    modal = modal
                        .child(
                            option(
                                "security-device",
                                "icons/smartphone.svg",
                                "Use another device",
                                "Confirm with a phone or computer where you're already signed in.",
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.guard = Guard::Waiting;
                                this.command(Command::Verify);
                                cx.notify();
                            })),
                        )
                        .child(
                            option(
                                "security-key",
                                "icons/key-round.svg",
                                "Enter your recovery key",
                                "The key or passphrase you saved when you set up encryption.",
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.guard = Guard::Key;
                                this.field.update(cx, |input, cx| {
                                    input.set_text("", cx);
                                    input.set_hint("huddle-recovery-hint", cx);
                                });
                                focus(&this.field, window, cx);
                                cx.notify();
                            })),
                        );
                }
                if recovery == Recovery::Disabled {
                    modal = modal.child(
                        option(
                            "security-setup",
                            "icons/shield-check.svg",
                            "Set up a recovery key",
                            "Get a key that unlocks your messages if you lose this device.",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::SetUpRecovery);
                            this.notify("Setting up your recovery key…", false, cx);
                        })),
                    );
                }
                modal.into_any_element()
            }
            Guard::Key | Guard::Recovering => {
                let busy = self.guard == Guard::Recovering;
                Modal::new("dialog-security-key", "Enter your recovery key").w(px(440.))
                    .detail("It looks like a long list of letters and numbers in groups of four. A passphrase works too.")
                    .on_dismiss(close)
                    .child(
                        div()
                            .flex()
                            .h(px(40.))
                            .on_action(cx.listener(|this, _: &Submit, _, cx| this.confirm(cx)))
                            .child(self.field.clone()),
                    )
                    .action(back)
                    .action(
                        button("security-unlock", if busy { "Unlocking…" } else { "Unlock" })
                            .primary()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    )
                    .into_any_element()
            }
            Guard::Waiting => Modal::new("dialog-security-wait", "Check your other device").w(px(440.))
                .detail("Open Element or another Matrix app where you're signed in, and accept the verification request.")
                .on_dismiss(close)
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .py_4()
                        .child(icon("icons/smartphone.svg", px(48.), theme.primary)),
                )
                .action(button("security-cancel", "Cancel").ghost().on_click(cx.listener(|this, _, _, cx| {
                    this.command(Command::CancelVerify);
                    this.guard = Guard::Menu;
                    cx.notify();
                })))
                .into_any_element(),
            Guard::Incoming => Modal::new("dialog-security-incoming", "Verify this device?").w(px(440.))
                .detail("Another device signed in to your account wants to verify this one. Accept only if you started it.")
                .on_dismiss(close)
                .action(button("security-decline", "Decline").ghost().on_click(cx.listener(|this, _, _, cx| {
                    this.command(Command::CancelVerify);
                    this.dialog = None;
                    cx.notify();
                })))
                .action(button("security-accept", "Accept").primary().on_click(cx.listener(|this, _, _, cx| {
                    this.command(Command::AcceptVerify);
                    this.guard = Guard::Waiting;
                    cx.notify();
                })))
                .into_any_element(),
            Guard::Emojis(emojis) => Modal::new("dialog-security-emoji", "Do these match?").w(px(440.))
                .detail("Check that the same emoji appear in the same order on your other device.")
                .on_dismiss(close)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_3()
                        .py_2()
                        .children(emojis.into_iter().map(|(symbol, name)| {
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_1()
                                .w(px(64.))
                                .child(div().text_size(px(32.)).child(symbol))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.muted_foreground)
                                        .child(name),
                                )
                        })),
                )
                .action(button("security-mismatch", "They don't match").ghost().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.command(Command::Match(false));
                        this.guard = Guard::Failed("The emoji didn't match, so verification was stopped.".into());
                        cx.notify();
                    },
                )))
                .action(button("security-match", "They match").primary().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.command(Command::Match(true));
                        this.guard = Guard::Waiting;
                        cx.notify();
                    },
                )))
                .into_any_element(),
            Guard::Done => Modal::new("dialog-security-done", "You're all set").w(px(440.))
                .detail("This device is verified. Older encrypted messages will unlock over the next few moments.")
                .on_dismiss(close)
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .py_2()
                        .child(icon("icons/shield-check.svg", px(48.), green())),
                )
                .action(done)
                .into_any_element(),
            Guard::Failed(reason) => Modal::new("dialog-security-failed", "Verification stopped").w(px(440.))
                .detail(reason)
                .on_dismiss(close)
                .action(back)
                .into_any_element(),
            Guard::NewKey(key) => {
                let copied = key.clone();
                Modal::new("dialog-security-new-key", "Save your recovery key").w(px(440.))
                    .detail("Keep it somewhere safe, like a password manager. You'll need it to read your messages on a new device.")
                    .on_dismiss(close)
                    .child(
                        div()
                            .p_3()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .border_1()
                            .border_color(theme.border)
                            .font_family(if cfg!(target_os = "windows") { "Consolas" } else { "Menlo" })
                            .text_color(theme.foreground)
                            .child(key),
                    )
                    .action(button("security-copy-key", "Copy").ghost().on_click(cx.listener(
                        move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            this.notify("Recovery key copied", false, cx);
                        },
                    )))
                    .action(done)
                    .into_any_element()
            }
        }
    }

    fn render_person(&self, person: &Person, cx: &mut Context<Self>) -> AnyElement {
        let mine = self.me.as_ref().is_some_and(|me| me.id == person.id);
        let person = person.clone();
        let title = match person.name.trim().is_empty() {
            true => person.id.to_string(),
            false => person.name.clone(),
        };
        let face = self.face(
            person.id.as_str(),
            &person.name,
            person.avatar.as_ref(),
            px(96.),
        );
        let copied_id = person.id.to_string();
        Modal::new("dialog-person", title)
            .w(px(520.))
            .detail(person.id.to_string())
            .close_button()
            .on_dismiss(cx.listener(|this, _: &(), _, cx| {
                this.person = None;
                cx.notify();
            }))
            .child(div().flex().justify_center().py_2().child(face))
            .child(self.public_profile_body(&person.id, cx))
            .action(
                Button::new("person-copy-id")
                    .ghost()
                    .label("Copy ID")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copied_id.clone()));
                    }),
            )
            .action(
                Button::new("person-refresh")
                    .ghost()
                    .label("Refresh")
                    .disabled(self.person_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_person(cx))),
            )
            .action(match mine {
                true => Button::new("person-edit")
                    .primary()
                    .label("Edit profile")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.person = None;
                        this.show_dialog(Dialog::Profile, window, cx);
                    })),
                false => Button::new("person-message")
                    .primary()
                    .label("Message")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = person.id.to_string();
                        this.person = None;
                        this.command(Command::Talk(id));
                        this.notify("Starting the conversation…", false, cx);
                    })),
            })
            .into_any_element()
    }

    fn render_dialog(&self, dialog: Dialog, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let close = cx.listener(|this, _: &(), _, cx| this.close_dialog(cx));
        let cancel = Button::new("dialog-cancel")
            .ghost()
            .label("Cancel")
            .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx)));
        let form = |example: &'static str, cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .on_action(cx.listener(|this, _: &Submit, _, cx| this.confirm(cx)))
                .child(div().flex().h(px(40.)).child(self.field.clone()))
                .child(
                    div()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child(example),
                )
        };
        let confirm = |label: &'static str, cx: &mut Context<Self>| {
            Button::new("dialog-confirm")
                .primary()
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))
        };

        match dialog {
            Dialog::RoomSettings => self.render_room_settings(cx),
            Dialog::Security => self.render_security(cx),
            Dialog::Choose => {
                let option = |id: &'static str, path: &'static str, title: &'static str, detail: &'static str, next: Dialog| {
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .hover(|style| style.border_color(theme.primary).bg(theme.secondary))
                        .child(icon(path, px(20.), theme.primary))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                                .child(
                                    div()
                                        .text_size(theme.text(Text::Small))
                                        .text_color(theme.muted_foreground)
                                        .child(detail),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.show_dialog(next, window, cx)))
                };
                Modal::new("dialog-choose", "What would you like to do?")
                    .close_button()
                    .on_dismiss(close)
                    .child(option("choose-join", "icons/search.svg", "Join a room", "Enter the address of an existing room.", Dialog::Join))
                    .child(option("choose-talk", "icons/user-round.svg", "Message someone", "Start a private conversation.", Dialog::Talk))
                    .child(option("choose-create", "icons/square-plus.svg", "Create a room", "Make a new place to chat.", Dialog::Create))
                    .into_any_element()
            }
            Dialog::Join => Modal::new("dialog-join", "Join a room")
                .detail("Room addresses start with # and look like #room:matrix.org. You'll often find them on websites, or a friend can share one with you.")
                .on_dismiss(close)
                .child(form("Tip: you can also paste a matrix.to link.", cx))
                .action(cancel)
                .action(confirm("Join", cx))
                .into_any_element(),
            Dialog::Create => Modal::new("dialog-create", "Create a room")
                .detail("Give your room a name. You can invite people once it's ready.")
                .on_dismiss(close)
                .child(form("For example: Weekend plans, Book club, Study group.", cx))
                .action(cancel)
                .action(confirm("Create", cx))
                .into_any_element(),
            Dialog::Talk => Modal::new("dialog-talk", "Message someone")
                .detail("Enter their Matrix username. It starts with @ and looks like @friend:matrix.org.")
                .on_dismiss(close)
                .child(form("Ask your friend for their username if you're not sure.", cx))
                .action(cancel)
                .action(confirm("Start chatting", cx))
                .into_any_element(),
            Dialog::Profile => self.render_profile(cx),
            Dialog::RemoveSpace => {
                let name = self.removing_space.as_ref().map(|(_, name)| name.as_str()).unwrap_or("this space");
                Modal::new("dialog-remove-space", format!("Remove {name}?"))
                    .detail("You'll leave this space and remove it from your sidebar. You'll stay joined to its rooms. Rejoining a private space may require an invitation.")
                    .on_dismiss(close).action(cancel)
                    .action(Button::new("dialog-remove-space-confirm").destructive().label("Remove space")
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))))
                    .into_any_element()
            }
            Dialog::Leave => {
                let name = self
                    .room
                    .as_ref()
                    .and_then(|room| self.summary(room))
                    .map_or_else(|| "this room".to_owned(), |room| room.name.clone());
                Modal::new("dialog-leave", format!("Leave {name}?"))
                    .detail("You'll stop getting messages from this room. You can rejoin later if it's public or someone invites you.")
                    .on_dismiss(close)
                    .action(cancel)
                    .action(
                        Button::new("dialog-leave-confirm")
                            .destructive()
                            .label("Leave")
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_line(&self, line: &Line, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let ringing = matches!(line, Line::Ringing { .. });
        let (title, name) = match line {
            Line::Ringing { name, .. } => ("Incoming call", name.clone()),
            Line::Live { name, .. } => ("On a call", name.clone()),
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(icon(
                if ringing {
                    "icons/phone.svg"
                } else {
                    "icons/video.svg"
                },
                px(16.),
                theme.foreground,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(
                        div()
                            .truncate()
                            .text_size(theme.text(Text::Small))
                            .text_color(theme.muted_foreground)
                            .child(name),
                    ),
            )
            .child(div().flex_1())
            .when(ringing, |row| {
                row.child(
                    Button::new("call-decline")
                        .ghost()
                        .small()
                        .label("Decline")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(Line::Ringing { room, event, .. }) = this.line.take() {
                                this.command(Command::DeclineCall { room, event });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("call-answer")
                        .primary()
                        .small()
                        .label("Answer")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let Some(Line::Ringing { room, name, .. }) = this.line.clone() else {
                                return;
                            };
                            let direct = this
                                .summary(&room)
                                .is_some_and(|room| room.kind == RoomKind::Direct);
                            this.line = Some(Line::Live {
                                room: room.clone(),
                                name,
                            });
                            this.command(Command::AnswerCall {
                                room: room.clone(),
                                direct,
                            });
                            this.open(room, window, cx);
                        })),
                )
            })
            .when(!ringing, |row| {
                row.child(
                    Button::new("call-hangup")
                        .destructive()
                        .small()
                        .label("Hang up")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.line = None;
                            this.command(Command::HangUp);
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}

impl Render for MatrixClient {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !window.is_window_active() {
            self.selection.finish();
        }
        self.prepare_selection();
        if self.selection.autoscrolling() {
            let list = if self.selection.thread() {
                self.thread_list.clone()
            } else {
                self.list.clone()
            };
            let bounds = list.viewport_bounds();
            let pointer = window.mouse_position();
            let edge = px(28.);
            let speed = if pointer.y < bounds.top() + edge {
                -px(8.)
            } else if pointer.y > bounds.bottom() - edge {
                px(8.)
            } else {
                px(0.)
            };
            if speed != px(0.) && bounds.size.height > px(0.) {
                let this = cx.weak_entity();
                window.on_next_frame(move |window, cx| {
                    this.update(cx, |this, cx| {
                        if this.selection.dragging() {
                            let old = list.logical_scroll_top();
                            list.scroll_by(speed);
                            let new = list.logical_scroll_top();
                            this.extend_selection(window.mouse_position());
                            if old.item_ix != new.item_ix
                                || old.offset_in_item != new.offset_in_item
                            {
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                });
            }
        }
        self.own_read_targets = [None, None];
        if let (Some(timeline), Some(me)) = (
            self.room.as_ref().and_then(|id| self.timelines.get(id)),
            self.me.as_ref(),
        ) {
            self.own_read_targets[0] = timeline
                .messages
                .iter()
                .rev()
                .find(|message| message.sender == me.id && message.id.is_some())
                .and_then(|message| message.id.clone());
            if let Some(root) = &self.thread {
                self.own_read_targets[1] = timeline
                    .threads
                    .get(root)
                    .and_then(|messages| {
                        messages
                            .iter()
                            .rev()
                            .find(|message| message.sender == me.id && message.id.is_some())
                    })
                    .and_then(|message| message.id.clone())
                    .or_else(|| {
                        timeline
                            .find(root)
                            .filter(|message| message.sender == me.id)
                            .and_then(|message| message.id.clone())
                    });
            }
        }
        self.release_player_texture(window);
        self.release_retired_images(cx);
        self.restore_history_anchor(window, cx);
        self.wire_composer(cx);
        if let Some(target) = self.pending_focus.take() {
            let input = match target {
                Focus::Draft => &self.draft,
                Focus::Step => &self.step_field,
            };
            focus(input, window, cx);
        }
        let theme = *cx.theme();
        let viewing = self.viewing.clone();
        let content = match self.phase {
            Phase::Starting => self.render_starting(cx),
            Phase::SignedOut | Phase::SigningIn => self.render_sign_in(cx),
            Phase::SignedIn => {
                let room = self
                    .room
                    .as_ref()
                    .and_then(|room| self.summary(room))
                    .filter(|room| !room.invite)
                    .cloned();
                let sidebar = self.on_home() || self.space.is_some();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when_some(self.line.clone(), |column, line| {
                        column.child(self.render_line(&line, cx))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_rail(cx))
                            .child(slide(
                                self.render_channels(cx).into_any_element(),
                                "channels",
                                sidebar,
                                CHANNELS,
                                false,
                            ))
                            .child(match room {
                                Some(room) => self.render_chat(&room, cx),
                                None => self.render_welcome(cx),
                            })
                            .when_some(self.dialog, |layout, dialog| {
                                layout.child(self.render_dialog(dialog, cx))
                            })
                            .when_some(self.person.clone(), |layout, person| {
                                layout.child(self.render_person(&person, cx))
                            })
                            .when_some(self.readers_view.clone(), |layout, selected| {
                                layout.child(self.render_readers(&selected, cx))
                            }),
                    )
                    .into_any_element()
            }
        };

        div()
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .text_size(theme.text(Text::Body))
            .text_color(theme.foreground)
            .track_focus(&self.text_focus)
            .key_context("MessageText")
            .on_action(cx.listener(|this, _: &ui::SelectAll, _, cx| {
                this.selection.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.selection.clear() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_mark(cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.selection.dragging() {
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.selection.finish();
                    } else {
                        this.extend_selection(event.position);
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if this.selection.dragging() {
                        this.extend_selection(event.position);
                        this.selection.finish();
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.selection.clear() {
                        cx.notify();
                    }
                }),
            )
            .child(content)
            .when_some(viewing, |root, view| {
                root.child(self.render_viewer(&view, window, cx))
            })
            .when(self.player.is_some(), |root| {
                root.child(self.render_player(window, cx))
            })
            .children(self.render_download_menu(cx))
            .children(self.render_download_panel(window, cx))
            .when_some(self.notice.clone(), |root, (text, failed)| {
                let toast =
                    Toast::new("huddle-notice", text).on_dismiss(cx.listener(|this, _, _, cx| {
                        this.notice = None;
                        cx.notify();
                    }));
                root.child(div().absolute().bottom_6().right_6().child(if failed {
                    toast.failed()
                } else {
                    toast
                }))
            })
    }
}

/// Preserve a message's position when content above it within its row changes height.
fn rebase_history_anchor(
    mut anchor: gpui::ListOffset,
    old_height: Pixels,
    mut measure: impl FnMut(usize) -> Pixels,
) -> gpui::ListOffset {
    anchor.offset_in_item += measure(anchor.item_ix) - old_height;
    // The viewport may have started inside a header that has now moved to an older row.
    while anchor.offset_in_item < px(0.) && anchor.item_ix > 0 {
        anchor.item_ix -= 1;
        anchor.offset_in_item += measure(anchor.item_ix);
    }
    anchor
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_event_id, owned_user_id};

    #[test]
    fn history_keeps_message_pixels_stationary_when_row_headers_change() {
        // The old first message ends at y=old_height-offset. Prepending rows and
        // removing its history/date/sender headers must leave that y unchanged.
        for (old_height, offset, heights) in [
            (180., 140., vec![80., 60., 100.]),
            (180., 10., vec![80., 60., 100.]),
            (240., 0., vec![50., 60., 80., 100.]),
            (100., 35., vec![80., 60., 160.]),
            (100., 35., vec![100.]),
        ] {
            let message_index = heights.len() - 1;
            let anchor = rebase_history_anchor(
                gpui::ListOffset {
                    item_ix: message_index,
                    offset_in_item: px(offset),
                },
                px(old_height),
                |index| px(heights[index]),
            );
            let scroll_top =
                heights[..anchor.item_ix].iter().sum::<f32>() + f32::from(anchor.offset_in_item);
            let message_bottom = heights.iter().sum::<f32>() - scroll_top;
            assert_eq!(message_bottom, old_height - offset);
            assert!(anchor.offset_in_item >= px(0.));
        }
    }

    fn message(id: &str, body: &str) -> Message {
        Message {
            id: Some(OwnedEventId::try_from(id).unwrap()),
            local: None,
            sender: owned_user_id!("@a:b.c"),
            sender_name: "a".into(),
            avatar: None,
            body: body.into(),
            rich: rich::plain(body),
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

    #[test]
    fn folds_edits_reactions_and_deletions() {
        let mut timeline = Timeline {
            messages: vec![message("$one", "hi"), message("$two", "there")],
            ..Timeline::default()
        };
        let edit = Event::Edit {
            target: owned_event_id!("$one"),
            sender: owned_user_id!("@a:b.c"),
            body: "hello".into(),
            rich: rich::plain("hello"),
        };
        assert!(matches!(
            timeline.relate(edit),
            Relate::Changed(Change::Updated(Place::Main(0)))
        ));
        assert_eq!(timeline.messages[0].body, "hello");
        assert!(timeline.messages[0].edited);

        let react = Event::React {
            target: owned_event_id!("$two"),
            key: "👍".into(),
            sender: owned_user_id!("@b:b.c"),
            id: owned_event_id!("$r"),
        };
        assert!(matches!(timeline.relate(react), Relate::Changed(_)));
        assert_eq!(timeline.messages[1].reactions[0].senders.len(), 1);
        assert!(matches!(
            timeline.relate(Event::Redact(owned_event_id!("$r"))),
            Relate::Changed(_)
        ));
        assert!(timeline.messages[1].reactions.is_empty());

        assert!(matches!(
            timeline.relate(Event::Redact(owned_event_id!("$one"))),
            Relate::Changed(Change::Removed(Place::Main(0), _))
        ));
        assert_eq!(timeline.messages.len(), 1);

        let late = Event::React {
            target: owned_event_id!("$zero"),
            key: "🎉".into(),
            sender: owned_user_id!("@b:b.c"),
            id: owned_event_id!("$late"),
        };
        let Relate::Orphan(late) = timeline.relate(late) else {
            panic!()
        };
        timeline.keep(late);
        timeline.messages.insert(0, message("$zero", "old"));
        timeline.retry();
        assert!(timeline.orphans.is_empty());
        assert_eq!(timeline.messages[0].reactions[0].key, "🎉");
    }

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff(b"\x89PNG\r\n"), Some(ImageFormat::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8"), Some(ImageFormat::Webp));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn measures_png_and_jpeg() {
        let mut png = vec![0u8; 24];
        png[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        png[12..16].copy_from_slice(b"IHDR");
        png[16..20].copy_from_slice(&320u32.to_be_bytes());
        png[20..24].copy_from_slice(&200u32.to_be_bytes());
        assert_eq!(measure(&png), (320, 200));

        let jpeg = [
            0xFFu8, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x20, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(measure(&jpeg), (32, 16));
        assert_eq!(measure(b"hello"), (0, 0));
    }
}
