use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt as _;
use futures::channel::mpsc as ui;
use matrix_sdk::attachment::{
    AttachmentConfig, AttachmentInfo, BaseAudioInfo, BaseImageInfo, BaseVideoInfo,
};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::RawSyncOrStrippedState;
use matrix_sdk::encryption::recovery::RecoveryState;
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, Verification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::encryption::{BackupDownloadStrategy, EncryptionSettings, VerificationState};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
use matrix_sdk::room::reply::{EnforceThread, Reply as RoomReply};
use matrix_sdk::room::{IncludeRelations, MessagesOptions, RelationsOptions};
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::api::client::account::{register, request_registration_token_via_email};
use matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType;
use matrix_sdk::ruma::api::client::room::create_room;
use matrix_sdk::ruma::api::client::uiaa::{self, AuthData, AuthType, UiaaInfo};
use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::receipt::ReceiptThread;
use matrix_sdk::ruma::events::relation::{Annotation, RelationType, Thread};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::encrypted;
use matrix_sdk::ruma::events::room::message::{
    AddMentions, FormattedBody, ForwardThread, MessageFormat, MessageType,
    OriginalSyncRoomMessageEvent, Relation, ReplacementMetadata, ReplyMetadata, ReplyWithinThread,
    RoomMessageEventContent, TextMessageEventContent,
};
use matrix_sdk::ruma::events::room::redaction::SyncRoomRedactionEvent;
use matrix_sdk::ruma::events::space::child::SpaceChildEventContent;
use matrix_sdk::ruma::events::{
    AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent, SyncStateEvent,
};
use matrix_sdk::ruma::media::Method;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{
    ClientSecret, EventId, MilliSecondsSinceUnixEpoch, OwnedClientSecret, OwnedEventId,
    OwnedMxcUri, OwnedRoomId, OwnedRoomOrAliasId, OwnedSessionId, OwnedUserId, UInt, UserId,
};
use matrix_sdk::{Client, ClientBuilder, LoopCtrl, Room, RoomMemberships, RoomState};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::profile;
use super::receipts;
use super::rich::{self, Rich};

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;

#[path = "call.rs"]
mod call;

#[path = "room_admin.rs"]
pub mod admin;

const DEVICE_NAME: &str = "Huddle";
const HISTORY: u32 = 60;
const PAGE: usize = 20;
const MEMBERS: usize = 300;
const THREAD: u32 = 100;
pub const SIGN_UP: &str = "https://app.element.io/#/register";
const NO_IDENTITY: &str =
    "Your account has no other verified device yet. Use your recovery key instead, or set one up.";
const WRONG_KEY: &str = "That key didn't work. Check it and try again.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoomKind {
    Room,
    Direct,
    Space,
}

#[derive(Clone)]
pub struct RoomSummary {
    pub can_manage: bool,
    pub id: OwnedRoomId,
    pub name: String,
    pub explicit_name: Option<String>,
    pub can_edit_name: bool,
    pub can_edit_topic: bool,
    pub topic: Option<String>,
    pub avatar: Option<OwnedMxcUri>,
    pub kind: RoomKind,
    pub invite: bool,
    pub encrypted: bool,
    pub unread: u64,
    pub members: u64,
    pub children: Vec<OwnedRoomId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Text,
    Emote,
    Notice,
    Image,
    Attachment,
    Encrypted,
}

#[derive(Clone)]
pub struct Picture {
    pub mime: Option<String>,
    pub thumbnail: Option<MediaSource>,
    pub duration: Option<Duration>,
    pub key: String,
    pub source: MediaSource,
    pub width: u32,
    pub height: u32,
    pub name: String,
}

#[derive(Clone)]
pub struct Reaction {
    pub key: String,
    pub senders: Vec<(OwnedUserId, OwnedEventId)>,
}

#[derive(Clone)]
pub struct Message {
    pub id: Option<OwnedEventId>,
    pub local: Option<u64>,
    pub sender: OwnedUserId,
    pub sender_name: String,
    pub avatar: Option<OwnedMxcUri>,
    pub body: String,
    pub rich: Rich,
    pub kind: MessageKind,
    pub picture: Option<Picture>,
    pub timestamp: i64,
    pub edited: bool,
    pub reply: Option<OwnedEventId>,
    pub thread: Option<OwnedEventId>,
    pub replies: u64,
    pub reactions: Vec<Reaction>,
    pub mentioned: bool,
}

pub enum Event {
    Message(Message),
    Edit {
        target: OwnedEventId,
        sender: OwnedUserId,
        body: String,
        rich: Rich,
    },
    React {
        target: OwnedEventId,
        key: String,
        sender: OwnedUserId,
        id: OwnedEventId,
    },
    Redact(OwnedEventId),
}

#[derive(Clone)]
pub struct Member {
    pub id: OwnedUserId,
    pub name: String,
    pub avatar: Option<OwnedMxcUri>,
    pub power: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Unknown,
    Enabled,
    Disabled,
    Incomplete,
}

pub enum Verify {
    Incoming,
    Waiting,
    Emojis(Vec<(String, String)>),
    Done,
    Cancelled(String),
}

#[derive(Clone)]
pub enum Step {
    Token,
    Terms(Vec<(String, String)>),
    Captcha(String),
    Email,
    EmailSent(String),
    Website(String),
}

pub enum Answer {
    Token(String),
    Terms,
    Captcha,
    Email(String),
    EmailConfirmed,
}

pub enum Command {
    LoadSpaceNames(OwnedRoomId),
    LoadProfile {
        room: Option<OwnedRoomId>,
        ticket: u64,
    },
    LoadPublicProfile {
        user: OwnedUserId,
        room: Option<OwnedRoomId>,
        ticket: u64,
    },
    Typing {
        room: OwnedRoomId,
        active: bool,
    },
    LoadRoomAdmin(OwnedRoomId),
    RoomAdmin {
        room: OwnedRoomId,
        action: admin::Action,
    },
    RoomSettings {
        room: OwnedRoomId,
        name: Option<String>,
        topic: Option<String>,
    },
    SignIn {
        server: String,
        username: String,
        password: String,
    },
    SignUp {
        server: String,
        username: String,
        password: String,
    },
    Answer(Answer),
    CancelSignUp,
    SignOut,
    Open {
        room: OwnedRoomId,
        history: bool,
    },
    Older {
        room: OwnedRoomId,
        from: String,
    },
    Thread {
        room: OwnedRoomId,
        root: OwnedEventId,
    },
    Send {
        room: OwnedRoomId,
        body: String,
        local: u64,
        reply: Option<(OwnedEventId, OwnedUserId)>,
        thread: Option<(OwnedEventId, OwnedEventId)>,
    },
    Picture {
        room: OwnedRoomId,
        name: String,
        mime: String,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        caption: String,
        duration: Option<Duration>,
        thumbnail: Option<super::playback::Thumbnail>,
        local: u64,
        reply: Option<(OwnedEventId, OwnedUserId)>,
        thread: Option<(OwnedEventId, OwnedEventId)>,
    },
    Profile(profile::Edit),
    Edit {
        room: OwnedRoomId,
        event: OwnedEventId,
        body: String,
    },
    React {
        room: OwnedRoomId,
        event: OwnedEventId,
        key: String,
    },
    Redact {
        room: OwnedRoomId,
        event: OwnedEventId,
    },
    Read {
        room: OwnedRoomId,
        event: OwnedEventId,
    },
    VideoPreview {
        key: String,
        source: MediaSource,
        thumbnail: Option<MediaSource>,
    },
    ClearCache,
    Fetch {
        key: String,
        source: MediaSource,
        size: Option<(u32, u32, bool)>,
    },
    Join(String),
    Create(String),
    Talk(String),
    Accept(OwnedRoomId),
    Leave(OwnedRoomId),
    Verify,
    AcceptVerify,
    Match(bool),
    CancelVerify,
    Recover(String),
    SetUpRecovery,
    Call {
        room: OwnedRoomId,
        direct: bool,
        voice: bool,
    },
    AnswerCall {
        room: OwnedRoomId,
        direct: bool,
    },
    DeclineCall {
        room: OwnedRoomId,
        event: OwnedEventId,
    },
    HangUp,
}

pub enum Update {
    SpaceNames {
        user: OwnedUserId,
        names: Vec<(OwnedRoomId, String)>,
    },
    ProfileDetails {
        user: OwnedUserId,
        ticket: u64,
        details: std::result::Result<Box<profile::Details>, String>,
    },
    PublicProfile {
        user: OwnedUserId,
        viewer: OwnedUserId,
        ticket: u64,
        details: profile::Public,
    },
    Unread(Vec<(OwnedRoomId, u64)>),
    Receipts {
        room: OwnedRoomId,
        readers: Vec<receipts::Reader>,
    },
    Typing {
        room: OwnedRoomId,
        users: Vec<OwnedUserId>,
    },
    RoomAdminLoaded {
        room: OwnedRoomId,
        result: std::result::Result<Box<admin::Snapshot>, String>,
    },
    RoomAdminDone {
        room: OwnedRoomId,
        error: Option<String>,
    },
    RoomSettingsSaved {
        room: OwnedRoomId,
        name: Option<String>,
        topic: Option<String>,
        error: Option<String>,
    },
    SignedOut,
    SigningIn,
    SignInFailed(String),
    SignUp(Step),
    SignUpFailed(String),
    SignedIn {
        user: OwnedUserId,
        name: String,
        avatar: Option<OwnedMxcUri>,
    },
    Profile(profile::Saved),
    Rooms(Vec<RoomSummary>),
    History {
        room: OwnedRoomId,
        events: Vec<Event>,
        token: Option<String>,
        older: bool,
    },
    Thread {
        room: OwnedRoomId,
        root: OwnedEventId,
        events: Vec<Event>,
    },
    Members {
        room: OwnedRoomId,
        members: Vec<Member>,
    },
    Live {
        room: OwnedRoomId,
        event: Event,
    },
    Sent {
        room: OwnedRoomId,
        local: u64,
        event: Option<OwnedEventId>,
    },
    Opened(OwnedRoomId),
    CacheCleared {
        error: Option<String>,
    },
    VideoPreview {
        key: String,
        bytes: Option<Vec<u8>>,
        thumbnail: Option<Vec<u8>>,
    },
    Media {
        key: String,
        bytes: Option<Vec<u8>>,
    },
    Security {
        verified: bool,
        recovery: Recovery,
    },
    Verify(Verify),
    RecoveryKey(String),
    Recovered,
    Failed(String),
    Ring {
        room: OwnedRoomId,
        event: OwnedEventId,
    },
    Calling {
        room: OwnedRoomId,
    },
    CallEnded,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    homeserver: String,
    session: MatrixSession,
    #[serde(default)]
    store: Option<String>,
}

type Outbox = ui::UnboundedSender<Update>;

fn post(outbox: &Outbox, update: Update) {
    outbox.unbounded_send(update).ok();
}

pub fn start() -> (
    mpsc::UnboundedSender<Command>,
    ui::UnboundedReceiver<Update>,
) {
    let (commands, inbox) = mpsc::unbounded_channel();
    let (outbox, updates) = ui::unbounded();
    std::thread::Builder::new()
        .name("matrix".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to start the Matrix runtime");
            runtime.block_on(Worker::new(outbox).run(inbox));
        })
        .expect("failed to spawn the Matrix thread");
    (commands, updates)
}

fn home() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("Huddle"))
}

fn session_path() -> Option<PathBuf> {
    home().map(|home| home.join("session.json"))
}

fn store_path(name: &str) -> Result<PathBuf> {
    home()
        .map(|home| home.join(name))
        .ok_or_else(|| anyhow!("no data directory"))
}

fn fresh_store() -> String {
    format!("store-{}", chrono::Utc::now().timestamp_millis())
}

/// Removes the saved session and every local store, so the next account starts clean.
fn forget() {
    let Some(home) = home() else { return };
    std::fs::remove_file(home.join("session.json")).ok();
    let Ok(entries) = std::fs::read_dir(&home) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("store") {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
}

fn builder() -> ClientBuilder {
    let mut settings = EncryptionSettings::default();
    settings.auto_enable_cross_signing = true;
    settings.auto_enable_backups = true;
    settings.backup_download_strategy = BackupDownloadStrategy::OneShot;
    Client::builder().with_encryption_settings(settings)
}

fn persist(client: &Client, store: &str) -> Result<()> {
    let session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| anyhow!("the server did not return a session"))?;
    let saved = Saved {
        homeserver: client.homeserver().to_string(),
        session,
        store: Some(store.to_owned()),
    };
    if let Some(path) = session_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string(&saved)?)?;
    }
    Ok(())
}

#[derive(Default)]
struct Shared {
    request: Mutex<Option<VerificationRequest>>,
    sas: Mutex<Option<SasVerification>>,
    calls: Arc<call::Calls>,
    cache_work: tokio::sync::Mutex<()>,
}

impl Shared {
    fn request(&self) -> Option<VerificationRequest> {
        self.request.lock().ok().and_then(|request| request.clone())
    }

    fn sas(&self) -> Option<SasVerification> {
        self.sas.lock().ok().and_then(|sas| sas.clone())
    }

    fn set_request(&self, request: Option<VerificationRequest>) {
        if let Ok(mut slot) = self.request.lock() {
            *slot = request;
        }
    }

    fn set_sas(&self, sas: Option<SasVerification>) {
        if let Ok(mut slot) = self.sas.lock() {
            *slot = sas;
        }
    }
}

struct SignUp {
    client: Client,
    store: String,
    username: String,
    password: String,
    session: Option<String>,
    email: Option<(OwnedClientSecret, OwnedSessionId)>,
    attempt: u32,
}

struct Worker {
    outbox: Outbox,
    client: Option<Client>,
    tasks: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
    signup: Option<SignUp>,
    typing: Option<mpsc::UnboundedSender<(OwnedRoomId, bool)>>,
}

impl Worker {
    fn new(outbox: Outbox) -> Self {
        Self {
            outbox,
            client: None,
            tasks: Vec::new(),
            shared: Arc::default(),
            signup: None,
            typing: None,
        }
    }

    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Command>) {
        match self.restore().await {
            Ok(true) => {}
            Ok(false) => post(&self.outbox, Update::SignedOut),
            Err(error) => {
                log::warn!("matrix: could not restore the session: {error:#}");
                post(&self.outbox, Update::SignedOut);
            }
        }

        while let Some(command) = inbox.recv().await {
            match command {
                Command::SignIn {
                    server,
                    username,
                    password,
                } => {
                    self.signup = None;
                    post(&self.outbox, Update::SigningIn);
                    if let Err(error) = self.sign_in(&server, &username, &password).await {
                        log::warn!("matrix: sign in failed: {error:#}");
                        post(&self.outbox, Update::SignInFailed(sign_in_problem(&error)));
                    }
                }
                Command::SignUp {
                    server,
                    username,
                    password,
                } => {
                    if let Err(error) = self.sign_up(&server, &username, &password).await {
                        log::warn!("matrix: sign up failed: {error:#}");
                        post(&self.outbox, Update::SignUpFailed(sign_up_problem(&error)));
                    }
                }
                Command::Answer(answer) => {
                    if let Err(error) = self.answer(answer).await {
                        log::warn!("matrix: sign up step failed: {error:#}");
                        post(&self.outbox, Update::SignUpFailed(sign_up_problem(&error)));
                    }
                }
                Command::CancelSignUp => self.signup = None,
                Command::SignOut => self.sign_out().await,
                Command::Typing { room, active } => {
                    if let Some(typing) = &self.typing {
                        let _ = typing.send((room, active));
                    }
                }
                command => {
                    let Some(client) = self.client.clone() else {
                        continue;
                    };
                    let outbox = self.outbox.clone();
                    let shared = self.shared.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle(&client, &outbox, &shared, command).await {
                            log::warn!("matrix: {error:#}");
                            post(&outbox, Update::Failed(problem(&error)));
                        }
                    });
                }
            }
        }
    }

    async fn restore(&mut self) -> Result<bool> {
        let Some(path) = session_path() else {
            return Ok(false);
        };
        let Ok(json) = std::fs::read_to_string(&path) else {
            return Ok(false);
        };
        let saved: Saved = serde_json::from_str(&json)?;
        let store = saved.store.unwrap_or_else(|| "store".to_owned());
        let client = builder()
            .homeserver_url(&saved.homeserver)
            .sqlite_store(store_path(&store)?, None)
            .build()
            .await?;
        client.restore_session(saved.session).await?;
        self.begin(client).await;
        Ok(true)
    }

    async fn sign_in(&mut self, server: &str, username: &str, password: &str) -> Result<()> {
        let username = username.trim();
        let server = match username.strip_prefix('@').and_then(|id| id.split_once(':')) {
            Some((_, host)) => host,
            None => server.trim(),
        };
        let server = if server.is_empty() {
            "matrix.org"
        } else {
            server
        };

        forget();
        let store = fresh_store();
        let client = builder()
            .server_name_or_homeserver_url(server)
            .sqlite_store(store_path(&store)?, None)
            .build()
            .await
            .context("unreachable")?;
        client
            .matrix_auth()
            .login_username(username, password)
            .initial_device_display_name(DEVICE_NAME)
            .await?;
        persist(&client, &store)?;
        self.begin(client).await;
        Ok(())
    }

    async fn sign_up(&mut self, server: &str, username: &str, password: &str) -> Result<()> {
        let username = username.trim().trim_start_matches('@');
        let (username, server) = match username.split_once(':') {
            Some((name, host)) => (name, host),
            None => (username, server.trim()),
        };
        let server = if server.is_empty() {
            "matrix.org"
        } else {
            server
        };

        self.signup = None;
        forget();
        let store = fresh_store();
        let client = builder()
            .server_name_or_homeserver_url(server)
            .sqlite_store(store_path(&store)?, None)
            .build()
            .await
            .context("unreachable")?;

        if let Ok(metadata) = client.oauth().server_metadata().await {
            let page = metadata
                .issuer
                .join("register")
                .map_or_else(|_| SIGN_UP.to_owned(), |page| page.to_string());
            post(&self.outbox, Update::SignUp(Step::Website(page)));
            return Ok(());
        }

        self.signup = Some(SignUp {
            client,
            store,
            username: username.to_lowercase(),
            password: password.to_owned(),
            session: None,
            email: None,
            attempt: 0,
        });
        self.register(None).await
    }

    async fn answer(&mut self, answer: Answer) -> Result<()> {
        let Some(signup) = self.signup.as_mut() else {
            return Ok(());
        };
        let session = signup.session.clone();
        let auth = match answer {
            Answer::Token(token) => {
                let mut data = uiaa::RegistrationToken::new(token.trim().to_owned());
                data.session = session;
                AuthData::RegistrationToken(data)
            }
            Answer::Terms => {
                let mut data = uiaa::Terms::new();
                data.session = session;
                AuthData::Terms(data)
            }
            Answer::Captcha => AuthData::FallbackAcknowledgement(
                uiaa::FallbackAcknowledgement::new(session.unwrap_or_default()),
            ),
            Answer::Email(address) => {
                let secret = ClientSecret::new();
                signup.attempt += 1;
                let request = request_registration_token_via_email::v3::Request::new(
                    secret.clone(),
                    address.trim().to_owned(),
                    UInt::from(signup.attempt),
                );
                let response = signup.client.send(request).await?;
                signup.email = Some((secret, response.sid));
                post(&self.outbox, Update::SignUp(Step::EmailSent(address)));
                return Ok(());
            }
            Answer::EmailConfirmed => {
                let Some((secret, sid)) = signup.email.clone() else {
                    return Ok(());
                };
                let serde_json::Value::Object(data) = serde_json::json!({
                    "threepid_creds": { "sid": sid, "client_secret": secret },
                }) else {
                    return Ok(());
                };
                AuthData::new("m.login.email.identity", session, data)?
            }
        };
        self.register(Some(auth)).await
    }

    async fn register(&mut self, mut auth: Option<AuthData>) -> Result<()> {
        let Some(signup) = self.signup.as_mut() else {
            return Ok(());
        };
        for _ in 0..4 {
            let mut request = register::v3::Request::new();
            request.username = Some(signup.username.clone());
            request.password = Some(signup.password.clone());
            request.initial_device_display_name = Some(DEVICE_NAME.to_owned());
            request.auth = auth.take();
            let error = match signup.client.matrix_auth().register(request).await {
                Ok(_) => {
                    let SignUp { client, store, .. } = self.signup.take().expect("checked above");
                    persist(&client, &store)?;
                    self.begin(client).await;
                    return Ok(());
                }
                Err(error) => error,
            };
            let Some(info) = error.as_uiaa_response().cloned() else {
                return Err(error.into());
            };
            signup.session = info.session.clone();
            if info.auth_error.is_some() {
                post(
                    &self.outbox,
                    Update::SignUpFailed("That didn't work. Check it and try again.".into()),
                );
            }
            let step = match next_stage(&info) {
                Some(AuthType::Dummy) => {
                    let mut data = uiaa::Dummy::new();
                    data.session = info.session.clone();
                    auth = Some(AuthData::Dummy(data));
                    continue;
                }
                Some(AuthType::RegistrationToken) => Step::Token,
                Some(AuthType::Terms) => Step::Terms(policies(&info)),
                Some(AuthType::ReCaptcha) => {
                    let page = signup
                        .client
                        .homeserver()
                        .join(&format!(
                            "_matrix/client/v3/auth/m.login.recaptcha/fallback/web?session={}",
                            info.session.clone().unwrap_or_default()
                        ))
                        .map_or_else(|_| SIGN_UP.to_owned(), |page| page.to_string());
                    Step::Captcha(page)
                }
                Some(AuthType::EmailIdentity) => Step::Email,
                _ => Step::Website(SIGN_UP.to_owned()),
            };
            post(&self.outbox, Update::SignUp(step));
            return Ok(());
        }
        Err(anyhow!("the server kept asking for more steps"))
    }

    async fn sign_out(&mut self) {
        self.typing = None;
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.shared.calls.end();
        self.shared.set_request(None);
        self.shared.set_sas(None);
        if let Some(client) = self.client.take()
            && let Err(error) = client.logout().await
        {
            log::warn!("matrix: could not sign out on the server: {error}");
        }
        forget();
        post(&self.outbox, Update::SignedOut);
    }

    async fn begin(&mut self, client: Client) {
        // Serialize notices separately so start/stop cannot overtake one another,
        // and a slow typing request cannot delay messages or uploads.
        let (typing, mut notices) = mpsc::unbounded_channel::<(OwnedRoomId, bool)>();
        self.typing = Some(typing);
        let typing_client = client.clone();
        self.tasks.push(tokio::spawn(async move {
            while let Some((id, active)) = notices.recv().await {
                if let Some(room) = typing_client.get_room(&id)
                    && let Err(error) = room.typing_notice(active).await
                {
                    log::debug!("matrix: typing notice failed: {error}");
                }
            }
        }));

        let outbox = self.outbox.clone();
        client.add_event_handler(
            move |event: matrix_sdk::ruma::events::typing::SyncTypingEvent, room: Room| {
                let outbox = outbox.clone();
                async move {
                    let users = event
                        .content
                        .user_ids
                        .into_iter()
                        .filter(|user| user.as_str() != room.own_user_id().as_str())
                        .take(3)
                        .collect();
                    post(
                        &outbox,
                        Update::Typing {
                            room: room.room_id().to_owned(),
                            users,
                        },
                    );
                }
            },
        );
        if let Err(error) = client
            .media()
            .set_media_retention_policy(cache_policy())
            .await
        {
            log::warn!("Could not configure the media cache: {error}");
        } else if let Err(error) = client.media().clean().await {
            log::warn!("Could not trim the media cache: {error}");
        }
        let Some(user) = client.user_id().map(UserId::to_owned) else {
            return;
        };
        let account = client.account();
        let name = account
            .get_display_name()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| user.localpart().to_owned());
        let avatar = account.get_avatar_url().await.ok().flatten();
        post(&self.outbox, Update::SignedIn { user, name, avatar });

        let outbox = self.outbox.clone();
        let shared = self.shared.clone();
        client.add_event_handler(move |raw: Raw<AnySyncTimelineEvent>, room: Room| {
            let outbox = outbox.clone();
            let shared = shared.clone();
            async move {
                if let Some(event) = call::incoming(&raw)
                    && raw
                        .get_field::<OwnedUserId>("sender")
                        .ok()
                        .flatten()
                        .as_deref()
                        != Some(room.own_user_id())
                    && !shared.calls.busy()
                {
                    post(
                        &outbox,
                        Update::Ring {
                            room: room.room_id().to_owned(),
                            event,
                        },
                    );
                }
                if let Some(event) = parse(&room, &raw).await {
                    let room = room.room_id().to_owned();
                    post(&outbox, Update::Live { room, event });
                }
            }
        });

        let outbox = self.outbox.clone();
        let shared = self.shared.clone();
        client.add_event_handler(
            move |event: ToDeviceKeyVerificationRequestEvent, client: Client| {
                let outbox = outbox.clone();
                let shared = shared.clone();
                async move {
                    let Some(request) = client
                        .encryption()
                        .get_verification_request(&event.sender, &event.content.transaction_id)
                        .await
                    else {
                        return;
                    };
                    if !request.is_self_verification() {
                        return;
                    }
                    shared.set_request(Some(request));
                    post(&outbox, Update::Verify(Verify::Incoming));
                }
            },
        );

        let outbox = self.outbox.clone();
        client.add_event_handler(
            move |event: matrix_sdk::ruma::events::receipt::SyncReceiptEvent, room: Room| {
                let outbox = outbox.clone();
                async move {
                    let readers = receipts::live(&room, event.content).await;
                    if !readers.is_empty() {
                        post(
                            &outbox,
                            Update::Receipts {
                                room: room.room_id().to_owned(),
                                readers,
                            },
                        );
                    }
                }
            },
        );

        let outbox = self.outbox.clone();
        let watched = client.clone();
        self.tasks.push(tokio::spawn(async move {
            let encryption = watched.encryption();
            let mut verification = encryption.verification_state();
            let mut recovery = encryption.recovery().state_stream();
            security(&watched, &outbox);
            loop {
                tokio::select! {
                    Some(_) = verification.next() => security(&watched, &outbox),
                    Some(_) = recovery.next() => security(&watched, &outbox),
                    else => break,
                }
            }
        }));

        // Client-side counts include messages that do not trigger a notification.
        // The event cache also updates them after local and remote read receipts.
        let mut changes = client.room_info_notable_update_receiver();
        if let Err(error) = client.event_cache().subscribe() {
            log::warn!("matrix: could not start unread message tracking: {error}");
        }
        let outbox = self.outbox.clone();
        let watched = client.clone();
        self.tasks.push(tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
                // Coalesce a sync batch without reloading room names or members.
                tokio::time::sleep(Duration::from_millis(50)).await;
                while changes.try_recv().is_ok() {}
                post(
                    &outbox,
                    Update::Unread(
                        watched
                            .rooms()
                            .into_iter()
                            .map(|room| (room.room_id().to_owned(), room.num_unread_messages()))
                            .collect(),
                    ),
                );
            }
        }));
        post(&self.outbox, Update::Rooms(summaries(&client).await));

        let outbox = self.outbox.clone();
        let syncing = client.clone();
        self.tasks.push(tokio::spawn(async move {
            let settings = SyncSettings::default().timeout(Duration::from_secs(30));
            let result = syncing
                .sync_with_callback(settings, |_| {
                    let client = syncing.clone();
                    let outbox = outbox.clone();
                    async move {
                        post(&outbox, Update::Rooms(summaries(&client).await));
                        LoopCtrl::Continue
                    }
                })
                .await;
            if let Err(error) = result {
                log::warn!("matrix: sync stopped: {error}");
                if matches!(
                    error.client_api_error_kind(),
                    Some(ErrorKind::UnknownToken(_))
                ) {
                    forget();
                    post(&outbox, Update::SignedOut);
                } else {
                    post(
                        &outbox,
                        Update::Failed(
                            "Lost the connection to your server. Restart Huddle to reconnect."
                                .into(),
                        ),
                    );
                }
            }
        }));
        self.client = Some(client);
    }
}

fn security(client: &Client, outbox: &Outbox) {
    let encryption = client.encryption();
    let verified = encryption.verification_state().get() == VerificationState::Verified;
    let recovery = match encryption.recovery().state() {
        RecoveryState::Enabled => Recovery::Enabled,
        RecoveryState::Disabled => Recovery::Disabled,
        RecoveryState::Incomplete => Recovery::Incomplete,
        _ => Recovery::Unknown,
    };
    post(outbox, Update::Security { verified, recovery });
}

fn next_stage(info: &UiaaInfo) -> Option<AuthType> {
    let supported = |stage: &AuthType| {
        matches!(
            stage,
            AuthType::Dummy
                | AuthType::RegistrationToken
                | AuthType::Terms
                | AuthType::ReCaptcha
                | AuthType::EmailIdentity
        )
    };
    let effort = |stages: &[AuthType]| {
        stages
            .iter()
            .filter(|stage| matches!(stage, AuthType::ReCaptcha | AuthType::EmailIdentity))
            .count()
    };
    let flow = info
        .flows
        .iter()
        .filter(|flow| flow.stages.iter().all(supported))
        .min_by_key(|flow| effort(&flow.stages))?;
    flow.stages
        .iter()
        .find(|stage| !info.completed.contains(stage))
        .cloned()
}

fn policies(info: &UiaaInfo) -> Vec<(String, String)> {
    let Some(params) = &info.params else {
        return Vec::new();
    };
    let Ok(params) = serde_json::from_str::<serde_json::Value>(params.get()) else {
        return Vec::new();
    };
    let Some(policies) = params["m.login.terms"]["policies"].as_object() else {
        return Vec::new();
    };
    policies
        .values()
        .filter_map(|policy| {
            let policy = policy.as_object()?;
            let translation = policy
                .get("en")
                .or_else(|| policy.values().find(|value| value.is_object()))?;
            Some((
                translation["name"].as_str()?.to_owned(),
                translation["url"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn cache_policy() -> matrix_sdk::media::MediaRetentionPolicy {
    matrix_sdk::media::MediaRetentionPolicy::empty()
        .with_max_cache_size(Some(512 * 1024 * 1024))
        .with_max_file_size(Some(128 * 1024 * 1024))
        .with_last_access_expiry(Some(Duration::from_secs(30 * 24 * 60 * 60)))
        .with_cleanup_frequency(Some(Duration::from_secs(15 * 60)))
}

fn video_preview_request(source: &MediaSource) -> MediaRequestParameters {
    MediaRequestParameters {
        source: source.clone(),
        format: MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
            Method::Scale,
            UInt::from(640u32),
            UInt::from(640u32),
        )),
    }
}

async fn clear_media_cache(client: &Client, shared: &Shared) -> Result<()> {
    let _operation = shared.cache_work.lock().await;
    let media = client.media();
    let previous = media.media_retention_policy().await?;
    media
        .set_media_retention_policy(
            matrix_sdk::media::MediaRetentionPolicy::empty()
                .with_max_cache_size(Some(0))
                .with_max_file_size(Some(0)),
        )
        .await?;
    // The SDK retains protected media used by pending uploads. Account state,
    // message history, and encryption keys live in separate stores.
    let cleaned = media.clean().await;
    let restored = media.set_media_retention_policy(previous).await;
    cleaned?;
    restored?;
    Ok(())
}

async fn handle(
    client: &Client,
    outbox: &Outbox,
    shared: &Arc<Shared>,
    command: Command,
) -> Result<()> {
    match command {
        Command::LoadSpaceNames(room) => {
            if let Some(user) = client.user_id().map(ToOwned::to_owned) {
                let names = space_names(client, &room, Some(outbox)).await;
                post(outbox, Update::SpaceNames { user, names });
            }
        }
        Command::LoadProfile { room, ticket } => {
            let user = client
                .user_id()
                .ok_or_else(|| anyhow!("not signed in"))?
                .to_owned();
            let details = profile::load(client, room.as_ref())
                .await
                .map(Box::new)
                .map_err(|error| problem(&error));
            post(
                outbox,
                Update::ProfileDetails {
                    user,
                    ticket,
                    details,
                },
            );
        }
        Command::LoadPublicProfile { user, room, ticket } => {
            let viewer = client
                .user_id()
                .ok_or_else(|| anyhow!("not signed in"))?
                .to_owned();
            let details = profile::inspect(client, &user, room.as_ref()).await;
            post(
                outbox,
                Update::PublicProfile {
                    user,
                    viewer,
                    ticket,
                    details,
                },
            );
        }
        Command::LoadRoomAdmin(room) => {
            let result = admin::load(client, &room)
                .await
                .map(Box::new)
                .map_err(|error| problem(&error));
            post(outbox, Update::RoomAdminLoaded { room, result });
        }
        Command::RoomAdmin { room, action } => {
            let result = admin::apply(client, &room, action).await;
            let error = result.err().map(|error| {
                log::warn!("room settings: {error:#}");
                problem(&error)
            });
            post(outbox, Update::Rooms(summaries(client).await));
            post(
                outbox,
                Update::RoomAdminDone {
                    room: room.clone(),
                    error,
                },
            );
            let result = admin::load(client, &room)
                .await
                .map(Box::new)
                .map_err(|error| problem(&error));
            post(outbox, Update::RoomAdminLoaded { room, result });
        }
        Command::RoomSettings { room, name, topic } => {
            let result = save_room_settings(client, &room, name.as_deref(), topic.as_deref()).await;
            if result.is_err() {
                // A server may accept the name and reject the topic. Reconcile
                // any partial change instead of claiming the save was atomic.
                post(outbox, Update::Rooms(summaries(client).await));
            }
            post(
                outbox,
                Update::RoomSettingsSaved {
                    room,
                    name,
                    topic,
                    error: result.err().map(|error| problem(&error)),
                },
            );
        }
        Command::Open { room: id, history } => {
            let room = joined(client, &id)?;
            if history {
                let (events, token) = page(&room, None).await?;
                let readers = receipts::snapshot(&room, &events, None).await;
                let latest = events.iter().rev().find_map(|event| match event {
                    Event::Message(message) => message.id.clone(),
                    _ => None,
                });
                post(
                    outbox,
                    Update::History {
                        room: id.clone(),
                        events,
                        token,
                        older: false,
                    },
                );
                post(
                    outbox,
                    Update::Receipts {
                        room: id.clone(),
                        readers,
                    },
                );
                if let Some(latest) = latest {
                    read(&room, latest).await;
                }
            }

            let mut members = Vec::new();
            for member in room.members_no_sync(RoomMemberships::JOIN).await? {
                members.push(Member {
                    id: member.user_id().to_owned(),
                    name: member.name().to_owned(),
                    avatar: member.avatar_url().map(ToOwned::to_owned),
                    power: match member.normalized_power_level() {
                        matrix_sdk::ruma::events::room::power_levels::UserPowerLevel::Infinite => {
                            100
                        }
                        matrix_sdk::ruma::events::room::power_levels::UserPowerLevel::Int(
                            level,
                        ) => i64::from(level),
                        _ => 0,
                    },
                });
            }
            members.sort_by(|a, b| {
                b.power
                    .cmp(&a.power)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            members.truncate(MEMBERS);
            post(outbox, Update::Members { room: id, members });
        }
        Command::Older { room: id, from } => {
            let room = joined(client, &id)?;
            let (events, token) = page(&room, Some(from)).await?;
            let readers = receipts::snapshot(&room, &events, None).await;
            post(
                outbox,
                Update::History {
                    room: id.clone(),
                    events,
                    token,
                    older: true,
                },
            );
            post(outbox, Update::Receipts { room: id, readers });
        }
        Command::Thread { room: id, root } => {
            let room = joined(client, &id)?;
            let mut options = RelationsOptions::default();
            options.dir = Direction::Backward;
            options.limit = Some(UInt::from(THREAD));
            options.include_relations = IncludeRelations::RelationsOfType(RelationType::Thread);
            let relations = room.relations(root.clone(), options).await?;
            let mut events = Vec::new();
            for event in relations.chunk.iter().rev() {
                if let Some(event) = parse(&room, event.raw()).await {
                    events.push(event);
                }
            }
            let readers = receipts::snapshot(&room, &events, Some(&root)).await;
            let latest = events.iter().rev().find_map(|event| match event {
                Event::Message(message) => message.id.clone(),
                _ => None,
            });
            post(
                outbox,
                Update::Thread {
                    room: id.clone(),
                    root: root.clone(),
                    events,
                },
            );
            post(outbox, Update::Receipts { room: id, readers });
            if let Some(latest) = latest {
                if let Err(error) = room
                    .send_single_receipt(ReceiptType::Read, ReceiptThread::Thread(root), latest)
                    .await
                {
                    log::debug!("matrix: could not send thread read receipt: {error}");
                }
            }
        }
        Command::Send {
            room: id,
            body,
            local,
            reply,
            thread,
        } => {
            let room = joined(client, &id)?;
            let mut content = RoomMessageEventContent::text_markdown(&body);
            if let Some((event, sender)) = &reply
                && thread.is_none()
            {
                content = content.make_reply_to(
                    ReplyMetadata::new(event, sender, None),
                    ForwardThread::No,
                    AddMentions::Yes,
                );
            }
            if let Some((root, latest)) = thread {
                content.relates_to = Some(Relation::Thread(match reply {
                    Some((event, _)) => Thread::reply(root, event),
                    None => Thread::plain(root, latest),
                }));
            }
            match room.send(content).await {
                Ok(sent) => post(
                    outbox,
                    Update::Sent {
                        room: id,
                        local,
                        event: Some(sent.response.event_id),
                    },
                ),
                Err(error) => {
                    post(
                        outbox,
                        Update::Sent {
                            room: id,
                            local,
                            event: None,
                        },
                    );
                    return Err(anyhow::Error::from(error).context("send"));
                }
            }
        }
        Command::Picture {
            room: id,
            name,
            mime,
            bytes,
            width,
            height,
            caption,
            duration,
            thumbnail,
            local,
            reply,
            thread,
        } => {
            let room = joined(client, &id)?;
            let mime: mime::Mime = mime.parse().context("Invalid attachment type")?;
            let size = UInt::try_from(bytes.len() as u64).ok();
            let info = match mime.type_() {
                mime::VIDEO => AttachmentInfo::Video(BaseVideoInfo {
                    width: (width > 0).then_some(UInt::from(width)),
                    height: (height > 0).then_some(UInt::from(height)),
                    duration,
                    size,
                    ..BaseVideoInfo::default()
                }),
                mime::AUDIO => AttachmentInfo::Audio(BaseAudioInfo {
                    duration,
                    size,
                    ..BaseAudioInfo::default()
                }),
                _ => AttachmentInfo::Image(BaseImageInfo {
                    width: (width > 0).then_some(UInt::from(width)),
                    height: (height > 0).then_some(UInt::from(height)),
                    size,
                    ..BaseImageInfo::default()
                }),
            };
            let mut config =
                AttachmentConfig::new()
                    .info(info)
                    .thumbnail(
                        thumbnail.map(|thumbnail| matrix_sdk::attachment::Thumbnail {
                            size: UInt::try_from(thumbnail.bytes.len() as u64).unwrap_or(UInt::MAX),
                            data: thumbnail.bytes,
                            content_type: mime::IMAGE_JPEG,
                            width: UInt::from(thumbnail.width),
                            height: UInt::from(thumbnail.height),
                        }),
                    );
            if !caption.is_empty() {
                config = config.caption(Some(TextMessageEventContent::markdown(caption)));
            }
            if let Some((_, latest)) = thread {
                let (event_id, within) = match reply {
                    Some((event, _)) => (event, ReplyWithinThread::Yes),
                    None => (latest, ReplyWithinThread::No),
                };
                config = config.reply(Some(RoomReply {
                    event_id,
                    enforce_thread: EnforceThread::Threaded(within),
                    add_mentions: AddMentions::Yes,
                }));
            } else if let Some((event, _)) = reply {
                config = config.reply(Some(RoomReply {
                    event_id: event,
                    enforce_thread: EnforceThread::Unthreaded,
                    add_mentions: AddMentions::Yes,
                }));
            }
            match room
                .send_attachment(name, &mime, bytes, config)
                .store_in_cache()
                .await
            {
                Ok(sent) => post(
                    outbox,
                    Update::Sent {
                        room: id,
                        local,
                        event: Some(sent.event_id),
                    },
                ),
                Err(error) => {
                    post(
                        outbox,
                        Update::Sent {
                            room: id,
                            local,
                            event: None,
                        },
                    );
                    return Err(anyhow::Error::from(error).context("upload"));
                }
            }
        }
        Command::Profile(edit) => {
            let saved = profile::save(client, edit).await?;
            post(outbox, Update::Profile(saved));
        }
        Command::Edit {
            room: id,
            event,
            body,
        } => {
            let room = joined(client, &id)?;
            let content = RoomMessageEventContent::text_markdown(&body)
                .make_replacement(ReplacementMetadata::new(event, None));
            room.send(content).await?;
        }
        Command::React {
            room: id,
            event,
            key,
        } => {
            let room = joined(client, &id)?;
            room.send(ReactionEventContent::new(Annotation::new(event, key)))
                .await?;
        }
        Command::Redact { room: id, event } => {
            let room = joined(client, &id)?;
            room.redact(&event, None, None).await?;
        }
        Command::Read { room: id, event } => {
            if let Some(room) = client.get_room(&id) {
                read(&room, event).await;
            }
        }
        Command::ClearCache => {
            let result = clear_media_cache(client, shared).await;
            if let Err(error) = &result {
                log::warn!("Could not clear the media cache: {error}");
            }
            post(
                outbox,
                Update::CacheCleared {
                    error: result
                        .err()
                        .map(|_| "The cache couldn't be cleared. Try again.".into()),
                },
            );
        }
        Command::VideoPreview {
            key,
            source,
            thumbnail,
        } => {
            let media = client.media();
            let request = video_preview_request(&source);
            let mut preview = client
                .media_store()
                .lock()
                .await?
                .get_media_content(&request)
                .await?;
            let cached = preview.is_some();
            if let Some(source) = thumbnail.filter(|_| !cached) {
                if let Ok(bytes) = media
                    .get_media_content(
                        &MediaRequestParameters {
                            source,
                            format: MediaFormat::File,
                        },
                        true,
                    )
                    .await
                {
                    // A broken advertised thumbnail must not prevent local extraction.
                    preview = tokio::task::spawn_blocking(move || {
                        image::load_from_memory(&bytes).ok().map(|_| bytes)
                    })
                    .await
                    .ok()
                    .flatten();
                }
            }
            if preview.is_none() {
                if let Ok(bytes) = media
                    .get_media_content(
                        &MediaRequestParameters {
                            source,
                            format: MediaFormat::File,
                        },
                        true,
                    )
                    .await
                {
                    preview = tokio::task::spawn_blocking(move || {
                        super::playback::thumbnail(bytes).ok().map(|p| p.bytes)
                    })
                    .await
                    .ok()
                    .flatten();
                }
            }
            if !cached && let Some(bytes) = &preview {
                if let Err(error) = client
                    .media_store()
                    .lock()
                    .await?
                    .add_media_content(
                        &request,
                        bytes.clone(),
                        matrix_sdk::media::store::IgnoreMediaRetentionPolicy::No,
                    )
                    .await
                {
                    log::debug!("Could not cache the video preview: {error}");
                }
            }
            post(
                outbox,
                Update::VideoPreview {
                    key,
                    // The SDK has already cached the original on disk. Scrolling
                    // past a video only needs its poster in the UI's memory.
                    bytes: None,
                    thumbnail: preview,
                },
            );
        }
        Command::Fetch { key, source, size } => {
            let media = client.media();
            let format = match (&source, size) {
                (MediaSource::Plain(_), Some((width, height, crop))) => {
                    MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
                        if crop { Method::Crop } else { Method::Scale },
                        UInt::from(width),
                        UInt::from(height),
                    ))
                }
                _ => MediaFormat::File,
            };
            let thumbnail = matches!(format, MediaFormat::Thumbnail(_));
            let mut bytes = media
                .get_media_content(
                    &MediaRequestParameters {
                        source: source.clone(),
                        format,
                    },
                    true,
                )
                .await;
            if bytes.is_err() && thumbnail {
                bytes = media
                    .get_media_content(
                        &MediaRequestParameters {
                            source,
                            format: MediaFormat::File,
                        },
                        true,
                    )
                    .await;
            }
            if let Err(error) = &bytes {
                log::debug!("matrix: could not load {key}: {error}");
            }
            post(
                outbox,
                Update::Media {
                    key,
                    bytes: bytes.ok(),
                },
            );
        }
        Command::Join(address) => {
            let address = room_address(client, &address)?;
            let room = client.join_room_by_id_or_alias(&address, &[]).await?;
            opened(client, outbox, room.room_id().to_owned()).await;
        }
        Command::Create(name) => {
            let mut request = create_room::v3::Request::new();
            request.name = Some(name);
            let room = client.create_room(request).await?;
            opened(client, outbox, room.room_id().to_owned()).await;
        }
        Command::Talk(user) => {
            let user = user_address(client, &user)?;
            let room = match client.get_dm_room(&user) {
                Some(room) => room,
                None => client.create_dm(&user).await?,
            };
            opened(client, outbox, room.room_id().to_owned()).await;
        }
        Command::Accept(id) => {
            let room = client
                .get_room(&id)
                .ok_or_else(|| anyhow!("unknown room"))?;
            room.join().await?;
            opened(client, outbox, id).await;
        }
        Command::Leave(id) => {
            let room = client
                .get_room(&id)
                .ok_or_else(|| anyhow!("unknown room"))?;
            room.leave().await?;
            post(outbox, Update::Rooms(summaries(client).await));
        }
        Command::Verify => {
            let user = client
                .user_id()
                .ok_or_else(|| anyhow!("not signed in"))?
                .to_owned();
            let identity = client
                .encryption()
                .request_user_identity(&user)
                .await?
                .ok_or_else(|| anyhow!(NO_IDENTITY))?;
            let request = identity.request_verification().await?;
            post(outbox, Update::Verify(Verify::Waiting));
            watch_request(outbox.clone(), shared.clone(), request);
        }
        Command::AcceptVerify => {
            let request = shared.request().ok_or_else(|| {
                anyhow!("The request has expired. Start again from the other device.")
            })?;
            request.accept().await?;
            post(outbox, Update::Verify(Verify::Waiting));
            watch_request(outbox.clone(), shared.clone(), request);
        }
        Command::Match(matches) => {
            let sas = shared
                .sas()
                .ok_or_else(|| anyhow!("The comparison has expired. Start again."))?;
            match matches {
                true => sas.confirm().await?,
                false => sas.mismatch().await?,
            }
        }
        Command::CancelVerify => {
            if let Some(sas) = shared.sas() {
                sas.cancel().await.ok();
            }
            if let Some(request) = shared.request() {
                request.cancel().await.ok();
            }
            shared.set_sas(None);
            shared.set_request(None);
        }
        Command::Recover(key) => {
            client
                .encryption()
                .recovery()
                .recover(key.trim())
                .await
                .map_err(|error| anyhow!(error).context(WRONG_KEY))?;
            security(client, outbox);
            post(outbox, Update::Recovered);
        }
        Command::SetUpRecovery => {
            let key = client.encryption().recovery().enable().await.map_err(|error| {
                anyhow!(error).context(
                    "Your account already has a recovery key. Enter it instead, or verify with another device.",
                )
            })?;
            post(outbox, Update::RecoveryKey(key));
            security(client, outbox);
        }
        Command::Call {
            room,
            direct,
            voice,
        } => {
            call::place(
                client,
                outbox,
                &shared.calls,
                call::Request {
                    room,
                    direct,
                    voice,
                    join: false,
                },
            )
            .await?;
        }
        Command::AnswerCall { room, direct } => {
            call::place(
                client,
                outbox,
                &shared.calls,
                call::Request {
                    room,
                    direct,
                    voice: false,
                    join: true,
                },
            )
            .await?;
        }
        Command::DeclineCall { room, event } => {
            let room = joined(client, &room)?;
            room.send_raw(
                "m.rtc.decline",
                serde_json::json!({
                    "m.relates_to": {
                        "rel_type": "m.reference",
                        "event_id": event,
                    }
                }),
            )
            .await?;
        }
        Command::HangUp => {
            shared.calls.end();
            post(outbox, Update::CallEnded);
        }
        Command::SignIn { .. }
        | Command::SignUp { .. }
        | Command::Answer(_)
        | Command::CancelSignUp
        | Command::Typing { .. }
        | Command::SignOut => {}
    }
    Ok(())
}

fn watch_request(outbox: Outbox, shared: Arc<Shared>, request: VerificationRequest) {
    shared.set_request(Some(request.clone()));
    tokio::spawn(async move {
        let mut changes = request.changes();
        let mut state = Some(request.state());
        loop {
            let current = match state.take() {
                Some(state) => state,
                None => match changes.next().await {
                    Some(state) => state,
                    None => break,
                },
            };
            match current {
                VerificationRequestState::Ready { .. } if request.we_started() => {
                    match request.start_sas().await {
                        Ok(Some(sas)) => watch_sas(outbox, shared, sas).await,
                        _ => post(
                            &outbox,
                            Update::Verify(Verify::Cancelled(
                                "Couldn't start comparing emoji.".into(),
                            )),
                        ),
                    }
                    return;
                }
                VerificationRequestState::Transitioned { verification } => {
                    if let Verification::SasV1(sas) = verification {
                        if !sas.we_started() {
                            sas.accept().await.ok();
                        }
                        watch_sas(outbox, shared, sas).await;
                    }
                    return;
                }
                VerificationRequestState::Done => {
                    post(&outbox, Update::Verify(Verify::Done));
                    return;
                }
                VerificationRequestState::Cancelled(info) => {
                    post(
                        &outbox,
                        Update::Verify(Verify::Cancelled(info.reason().to_owned())),
                    );
                    return;
                }
                _ => {}
            }
        }
    });
}

async fn watch_sas(outbox: Outbox, shared: Arc<Shared>, sas: SasVerification) {
    shared.set_sas(Some(sas.clone()));
    let mut changes = sas.changes();
    let mut state = Some(sas.state());
    loop {
        let current = match state.take() {
            Some(state) => state,
            None => match changes.next().await {
                Some(state) => state,
                None => break,
            },
        };
        match current {
            SasState::KeysExchanged {
                emojis: Some(emojis),
                ..
            } => {
                let emojis = emojis
                    .emojis
                    .iter()
                    .map(|emoji| (emoji.symbol.to_owned(), emoji.description.to_owned()))
                    .collect();
                post(&outbox, Update::Verify(Verify::Emojis(emojis)));
            }
            SasState::Done { .. } => {
                post(&outbox, Update::Verify(Verify::Done));
                break;
            }
            SasState::Cancelled(info) => {
                post(
                    &outbox,
                    Update::Verify(Verify::Cancelled(info.reason().to_owned())),
                );
                break;
            }
            _ => {}
        }
    }
    shared.set_sas(None);
    shared.set_request(None);
}

fn joined(client: &Client, id: &OwnedRoomId) -> Result<Room> {
    client.get_room(id).ok_or_else(|| anyhow!("unknown room"))
}

/// Pages backwards until enough visible messages arrive, oldest first.
async fn page(room: &Room, mut from: Option<String>) -> Result<(Vec<Event>, Option<String>)> {
    let mut batches = Vec::new();
    let mut shown = 0;
    for _ in 0..5 {
        let mut options = MessagesOptions::backward();
        options.from = from.clone();
        options.limit = UInt::from(HISTORY);
        let response = room.messages(options).await?;
        let mut batch = Vec::with_capacity(response.chunk.len());
        for event in response.chunk.iter().rev() {
            if let Some(event) = parse(room, event.raw()).await {
                if matches!(event, Event::Message(_)) {
                    shown += 1;
                }
                batch.push(event);
            }
        }
        batches.push(batch);
        from = response.end;
        if from.is_none() || shown >= PAGE {
            break;
        }
    }
    Ok((batches.into_iter().rev().flatten().collect(), from))
}

async fn opened(client: &Client, outbox: &Outbox, room: OwnedRoomId) {
    post(outbox, Update::Rooms(summaries(client).await));
    post(outbox, Update::Opened(room));
}

async fn read(room: &Room, event: OwnedEventId) {
    if let Err(error) = room
        .send_single_receipt(ReceiptType::Read, ReceiptThread::Unthreaded, event)
        .await
    {
        log::debug!("matrix: could not send a read receipt: {error}");
    }
}

fn own_server(client: &Client) -> String {
    client
        .user_id()
        .map(|user| user.server_name().to_string())
        .unwrap_or_else(|| "matrix.org".into())
}

fn room_address(client: &Client, input: &str) -> Result<OwnedRoomOrAliasId> {
    let input = input.trim();
    let input = input.strip_prefix("https://matrix.to/#/").unwrap_or(input);
    let input = input.split('?').next().unwrap_or(input);
    let address = match input.chars().next() {
        Some('#' | '!') if input.contains(':') => input.to_owned(),
        Some('#') => format!("{input}:{}", own_server(client)),
        _ if input.contains(':') => format!("#{input}"),
        _ => format!("#{input}:{}", own_server(client)),
    };
    OwnedRoomOrAliasId::try_from(address.as_str()).map_err(|_| {
        anyhow!("That doesn't look like a room address. Try something like #room:matrix.org.")
    })
}

fn user_address(client: &Client, input: &str) -> Result<OwnedUserId> {
    let input = input.trim();
    let input = input.strip_prefix("https://matrix.to/#/").unwrap_or(input);
    let input = input.strip_prefix('@').unwrap_or(input);
    let address = match input.contains(':') {
        true => format!("@{input}"),
        false => format!("@{input}:{}", own_server(client)),
    };
    OwnedUserId::try_from(address.as_str()).map_err(|_| {
        anyhow!("That doesn't look like a username. Try something like @friend:matrix.org.")
    })
}

async fn save_room_settings(
    client: &Client,
    id: &OwnedRoomId,
    name: Option<&str>,
    topic: Option<&str>,
) -> Result<()> {
    let room = joined(client, id)?;
    let user = client.user_id().ok_or_else(|| anyhow!("not signed in"))?;
    let levels = admin::fresh_levels(client, &room).await?;
    let (can_name, can_topic) = room_edit_permissions(&levels, user);
    anyhow::ensure!(
        (name.is_none() || can_name) && (topic.is_none() || can_topic),
        "You don't have permission to change these room settings."
    );
    if let Some(name) = name {
        room.set_name(name.to_owned()).await?;
    }
    if let Some(topic) = topic {
        room.set_room_topic(topic).await?;
    }
    Ok(())
}

fn room_edit_permissions(
    levels: &matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels,
    user: &UserId,
) -> (bool, bool) {
    use matrix_sdk::ruma::events::StateEventType;
    (
        levels.user_can_send_state(user, StateEventType::RoomName),
        levels.user_can_send_state(user, StateEventType::RoomTopic),
    )
}

async fn summaries(client: &Client) -> Vec<RoomSummary> {
    let mut rooms = Vec::new();
    for room in client.rooms() {
        let state = room.state();
        if !matches!(state, RoomState::Joined | RoomState::Invited) {
            continue;
        }
        let name = room
            .display_name()
            .await
            .map(|name| name.to_string())
            .unwrap_or_else(|_| room.room_id().to_string());
        let kind = match (room.is_space(), room.is_direct().await.unwrap_or(false)) {
            (true, _) => RoomKind::Space,
            (false, true) => RoomKind::Direct,
            (false, false) => RoomKind::Room,
        };
        let children = match kind {
            RoomKind::Space => space_children(&room).await,
            _ => Vec::new(),
        };
        let permissions = if state == RoomState::Joined {
            match (client.user_id(), room.power_levels().await) {
                (Some(user), Ok(levels)) => admin::Permissions::new(&levels, user),
                _ => admin::Permissions::default(),
            }
        } else {
            admin::Permissions::default()
        };
        rooms.push(RoomSummary {
            can_manage: permissions.any(),
            id: room.room_id().to_owned(),
            name,
            explicit_name: room.name(),
            can_edit_name: permissions.name,
            can_edit_topic: permissions.topic,
            topic: room.topic().filter(|topic| !topic.trim().is_empty()),
            avatar: room.avatar_url(),
            kind,
            invite: state == RoomState::Invited,
            encrypted: room.encryption_state().is_encrypted(),
            unread: room.num_unread_messages(),
            members: room.joined_members_count(),
            children,
        });
    }
    rooms.sort_by_key(|room| room.name.to_lowercase());
    rooms
}

async fn space_children(space: &Room) -> Vec<OwnedRoomId> {
    space_child_routes(space)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

async fn space_child_routes(
    space: &Room,
) -> Vec<(OwnedRoomId, Vec<matrix_sdk::ruma::OwnedServerName>)> {
    let Ok(events) = space
        .get_state_events_static::<SpaceChildEventContent>()
        .await
    else {
        return Vec::new();
    };
    events
        .into_iter()
        .filter_map(|event| match event {
            RawSyncOrStrippedState::Sync(raw) => match raw.deserialize().ok()? {
                SyncStateEvent::Original(event) if !event.content.via.is_empty() => {
                    Some((event.state_key, event.content.via))
                }
                _ => None,
            },
            RawSyncOrStrippedState::Stripped(_) => None,
        })
        .collect()
}

/// Space discovery includes metadata for children without joining them. Keep
/// earlier pages usable if a later page is unavailable or access is restricted.
async fn space_names(
    client: &Client,
    room: &OwnedRoomId,
    publish: Option<&Outbox>,
) -> Vec<(OwnedRoomId, String)> {
    use matrix_sdk::ruma::api::client::space::get_hierarchy;
    let mut names = Vec::new();
    let mut from = None;
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut request = get_hierarchy::v1::Request::new(room.clone());
        request.max_depth = Some(1u32.into());
        request.limit = Some(100u32.into());
        request.from = from;
        let response = match client
            .send(request)
            .with_request_config(
                matrix_sdk::config::RequestConfig::short_retry()
                    .retry_limit(0)
                    .timeout(Duration::from_secs(5)),
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                log::debug!("Space names unavailable for {room}: {error}");
                break;
            }
        };
        for chunk in response.rooms {
            let summary = chunk.summary;
            if summary.room_id == *room {
                continue;
            }
            let label = summary
                .name
                .filter(|name| !name.trim().is_empty())
                .or_else(|| summary.canonical_alias.map(|alias| alias.to_string()));
            if let Some(label) = label {
                names.push((summary.room_id, label));
            }
        }
        from = response.next_batch;
        let Some(token) = &from else {
            break;
        };
        if !seen.insert(token.clone()) {
            break;
        }
    }
    // Some homeservers reject hierarchy discovery or omit otherwise public
    // children. Preview those rooms through the servers advertised by the link.
    if let Some(parent) = client.get_room(room) {
        let known: std::collections::HashSet<_> = names.iter().map(|(id, _)| id.clone()).collect();
        let routes = space_child_routes(&parent).await;
        let mut previews = futures::stream::iter(
            routes
                .into_iter()
                .filter(|(id, _)| id != room && !known.contains(id))
                .map(|(id, via)| async move {
                    let address: OwnedRoomOrAliasId = id.clone().into();
                    let preview = tokio::time::timeout(
                        Duration::from_secs(30),
                        client.get_room_preview(&address, via),
                    )
                    .await;
                    let name = match preview {
                        Ok(Ok(preview)) => preview
                            .name
                            .filter(|name| !name.trim().is_empty())
                            .or_else(|| preview.canonical_alias.map(|alias| alias.to_string())),
                        Ok(Err(error)) => {
                            log::debug!("Linked room preview unavailable for {id}: {error}");
                            None
                        }
                        Err(_) => None,
                    };
                    (id, name)
                }),
        )
        .buffer_unordered(4);
        while let Some((id, name)) = previews.next().await {
            let Some(name) = name else {
                continue;
            };
            if let (Some(outbox), Some(user)) = (publish, client.user_id()) {
                post(
                    outbox,
                    Update::SpaceNames {
                        user: user.to_owned(),
                        names: vec![(id.clone(), name.clone())],
                    },
                );
            }
            names.push((id, name));
        }
    }
    names
}

fn millis(timestamp: MilliSecondsSinceUnixEpoch) -> i64 {
    i64::from(timestamp.0)
}

fn media_url(source: &MediaSource) -> String {
    match source {
        MediaSource::Plain(uri) => uri.to_string(),
        MediaSource::Encrypted(file) => file.url.to_string(),
    }
}

async fn base(
    room: &Room,
    sender: &UserId,
    id: &EventId,
    timestamp: MilliSecondsSinceUnixEpoch,
) -> Message {
    let member = room.get_member_no_sync(sender).await.ok().flatten();
    Message {
        id: Some(id.to_owned()),
        local: None,
        sender: sender.to_owned(),
        sender_name: member.as_ref().map_or_else(
            || sender.localpart().to_owned(),
            |member| member.name().to_owned(),
        ),
        avatar: member.and_then(|member| member.avatar_url().map(ToOwned::to_owned)),
        body: String::new(),
        rich: Rich::new(),
        kind: MessageKind::Text,
        picture: None,
        timestamp: millis(timestamp),
        edited: false,
        reply: None,
        thread: None,
        replies: 0,
        reactions: Vec::new(),
        mentioned: false,
    }
}

async fn parse(room: &Room, raw: &Raw<AnySyncTimelineEvent>) -> Option<Event> {
    let AnySyncTimelineEvent::MessageLike(event) = raw.deserialize().ok()? else {
        return None;
    };
    match event {
        AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(event)) => {
            from_message(room, &event).await
        }
        AnySyncMessageLikeEvent::RoomEncrypted(SyncMessageLikeEvent::Original(event)) => {
            let thread = match &event.content.relates_to {
                Some(encrypted::Relation::Thread(thread)) => Some(thread.event_id.clone()),
                Some(encrypted::Relation::Replacement(_) | encrypted::Relation::Annotation(_)) => {
                    return None;
                }
                _ => None,
            };
            let mut message =
                base(room, &event.sender, &event.event_id, event.origin_server_ts).await;
            message.kind = MessageKind::Encrypted;
            message.thread = thread;
            Some(Event::Message(message))
        }
        AnySyncMessageLikeEvent::Reaction(SyncMessageLikeEvent::Original(event)) => {
            Some(Event::React {
                target: event.content.relates_to.event_id,
                key: event.content.relates_to.key,
                sender: event.sender,
                id: event.event_id,
            })
        }
        AnySyncMessageLikeEvent::RoomRedaction(SyncRoomRedactionEvent::Original(event)) => {
            event.content.redacts.or(event.redacts).map(Event::Redact)
        }
        _ => None,
    }
}

fn mentions_user(content: &RoomMessageEventContent, user: Option<&UserId>) -> bool {
    content.mentions.as_ref().is_some_and(|mentions| {
        mentions.room || user.is_some_and(|user| mentions.user_ids.contains(user))
    })
}

async fn from_message(room: &Room, event: &OriginalSyncRoomMessageEvent) -> Option<Event> {
    let content = &event.content;
    if let Some(Relation::Replacement(replacement)) = &content.relates_to {
        let (body, rich, _, _) = describe(&replacement.new_content.msgtype);
        return Some(Event::Edit {
            target: replacement.event_id.clone(),
            sender: event.sender.clone(),
            body,
            rich,
        });
    }

    let mut message = base(room, &event.sender, &event.event_id, event.origin_server_ts).await;
    message.mentioned = mentions_user(content, room.client().user_id());
    match &content.relates_to {
        Some(Relation::Reply(reply)) => message.reply = Some(reply.in_reply_to.event_id.clone()),
        Some(Relation::Thread(thread)) => {
            message.thread = Some(thread.event_id.clone());
            if !thread.is_falling_back {
                message.reply = thread
                    .in_reply_to
                    .as_ref()
                    .map(|reply| reply.event_id.clone());
            }
        }
        _ => {}
    }

    let mut msgtype = &content.msgtype;
    if let Some(replace) = &event.unsigned.relations.replace
        && replace.sender == event.sender
        && let Some(Relation::Replacement(replacement)) = &replace.content.relates_to
    {
        msgtype = &replacement.new_content.msgtype;
        message.edited = true;
    }
    message.replies = event
        .unsigned
        .relations
        .thread
        .as_ref()
        .map_or(0, |thread| u64::from(thread.count));

    let (body, rich, kind, picture) = describe(msgtype);
    message.body = body;
    message.rich = rich;
    message.kind = kind;
    message.picture = picture;
    Some(Event::Message(message))
}

fn formatted(body: &str, formatted: Option<&FormattedBody>) -> Rich {
    match formatted.filter(|formatted| formatted.format == MessageFormat::Html) {
        Some(formatted) => {
            let rich = rich::html(&formatted.body);
            match rich.is_empty() {
                true => rich::plain(&without_reply(body)),
                false => rich,
            }
        }
        None => rich::plain(&without_reply(body)),
    }
}

fn size(value: Option<UInt>) -> u32 {
    value.map_or(0, |value| u32::try_from(u64::from(value)).unwrap_or(0))
}

fn describe(msgtype: &MessageType) -> (String, Rich, MessageKind, Option<Picture>) {
    match msgtype {
        MessageType::Text(text) => (
            without_reply(&text.body),
            formatted(&text.body, text.formatted.as_ref()),
            MessageKind::Text,
            None,
        ),
        MessageType::Notice(notice) => (
            without_reply(&notice.body),
            formatted(&notice.body, notice.formatted.as_ref()),
            MessageKind::Notice,
            None,
        ),
        MessageType::Emote(emote) => (
            emote.body.clone(),
            formatted(&emote.body, emote.formatted.as_ref()),
            MessageKind::Emote,
            None,
        ),
        MessageType::Image(image) => {
            let info = image.info.as_deref();
            let body = without_reply(&image.body);
            let name = image
                .filename
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| body.clone());
            let picture = Picture {
                mime: None,
                thumbnail: None,
                duration: None,
                key: format!("{}#image", media_url(&image.source)),
                source: image.source.clone(),
                width: size(info.and_then(|info| info.width)),
                height: size(info.and_then(|info| info.height)),
                name,
            };
            let caption = match &image.filename {
                Some(filename) if filename != &body => formatted(&body, image.formatted.as_ref()),
                _ => Rich::new(),
            };
            (body, caption, MessageKind::Image, Some(picture))
        }
        MessageType::Video(video) => {
            let (body, _, kind, mut picture) = playable(
                &video.body,
                video.filename.as_deref(),
                &video.source,
                video
                    .info
                    .as_ref()
                    .and_then(|info| info.mimetype.as_deref()),
            );
            if let Some(picture) = &mut picture {
                picture.mime = Some(
                    video
                        .info
                        .as_ref()
                        .and_then(|i| i.mimetype.clone())
                        .unwrap_or("video/mp4".into()),
                );
                if let Some(info) = &video.info {
                    picture.width = size(info.width);
                    picture.height = size(info.height);
                    picture.duration = info.duration;
                    picture.thumbnail = info.thumbnail_source.clone();
                }
            }
            let caption = if video.filename.as_ref().is_some_and(|name| name != &body) {
                formatted(&body, video.formatted.as_ref())
            } else {
                Rich::new()
            };
            (body, caption, kind, picture)
        }
        MessageType::Audio(audio) => playable(
            &audio.body,
            audio.filename.as_deref(),
            &audio.source,
            audio
                .info
                .as_ref()
                .and_then(|info| info.mimetype.as_deref()),
        ),
        MessageType::File(file) => {
            let mime = file.info.as_ref().and_then(|info| info.mimetype.as_deref());
            if mime.is_some_and(|mime| mime.starts_with("audio/") || mime.starts_with("video/"))
                || super::playback::mime(file.filename.as_deref().unwrap_or(&file.body)).is_some()
            {
                playable(&file.body, file.filename.as_deref(), &file.source, mime)
            } else {
                attachment(format!("Shared a file: {}", file.body))
            }
        }
        other => (
            other.body().to_owned(),
            rich::plain(other.body()),
            MessageKind::Text,
            None,
        ),
    }
}

fn playable(
    body: &str,
    filename: Option<&str>,
    source: &MediaSource,
    mime: Option<&str>,
) -> (String, Rich, MessageKind, Option<Picture>) {
    let name = filename.unwrap_or(body).to_owned();
    let mime = mime
        .filter(|m| m.starts_with("audio/") || m.starts_with("video/"))
        .or_else(|| super::playback::mime(&name))
        .unwrap_or("application/octet-stream")
        .to_owned();
    (
        body.to_owned(),
        Rich::new(),
        MessageKind::Attachment,
        Some(Picture {
            key: format!("{}#playback", media_url(source)),
            source: source.clone(),
            width: 0,
            height: 0,
            name,
            mime: Some(mime),
            thumbnail: None,
            duration: None,
        }),
    )
}

fn attachment(label: String) -> (String, Rich, MessageKind, Option<Picture>) {
    (
        label.clone(),
        rich::plain(&label),
        MessageKind::Attachment,
        None,
    )
}

fn without_reply(body: &str) -> String {
    if !body.starts_with("> ") {
        return body.to_owned();
    }
    let mut lines = body.lines().skip_while(|line| line.starts_with('>'));
    match lines.next() {
        Some(line) if line.is_empty() => lines.collect::<Vec<_>>().join("\n"),
        _ => body.to_owned(),
    }
}

fn kind(error: &anyhow::Error) -> Option<&ErrorKind> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<matrix_sdk::Error>())
        .and_then(matrix_sdk::Error::client_api_error_kind)
        .or_else(|| {
            error
                .chain()
                .find_map(|cause| cause.downcast_ref::<matrix_sdk::HttpError>())
                .and_then(matrix_sdk::HttpError::client_api_error_kind)
        })
}

fn unreachable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string() == "unreachable")
}

fn sign_in_problem(error: &anyhow::Error) -> String {
    if unreachable(error) {
        return "We couldn't reach that server. Check the address and your connection.".into();
    }
    match kind(error) {
        Some(ErrorKind::Forbidden) => {
            "That username and password don't match. Please try again.".into()
        }
        Some(ErrorKind::LimitExceeded(_)) => {
            "Too many attempts. Wait a moment and try again.".into()
        }
        Some(ErrorKind::UserDeactivated) => "This account has been deactivated.".into(),
        _ => "Couldn't sign in. Check your details and try again.".into(),
    }
}

fn sign_up_problem(error: &anyhow::Error) -> String {
    if unreachable(error) {
        return "We couldn't reach that server. Check the address and your connection.".into();
    }
    match kind(error) {
        Some(ErrorKind::UserInUse) => "That username is taken. Try another one.".into(),
        Some(ErrorKind::InvalidUsername) => {
            "Usernames can only use lowercase letters, numbers and . _ = - /".into()
        }
        Some(ErrorKind::WeakPassword) => "That password is too weak. Try a longer one.".into(),
        Some(ErrorKind::ThreepidInUse) => "That email is already used by another account.".into(),
        Some(ErrorKind::Forbidden) => {
            "This server isn't taking new accounts. Try matrix.org instead.".into()
        }
        Some(ErrorKind::LimitExceeded(_)) => {
            "Too many attempts. Wait a moment and try again.".into()
        }
        _ => "Couldn't create the account. Please try again.".into(),
    }
}

fn problem(error: &anyhow::Error) -> String {
    let friendly = [
        "That doesn't look",
        "That key didn't",
        "Your account",
        "Your server",
        "Some profile changes",
        "The request has",
        "The comparison has",
        "The call",
        "You don't have permission",
        "Enter a ",
        "Choose ",
        "That address ",
        "Room permissions ",
        "Invalid ",
        "Unsupported room ",
        "Spaces cannot ",
        "Join this room ",
        "Room was created, ",
    ];
    if let Some(message) = error.chain().find_map(|cause| {
        let text = cause.to_string();
        friendly
            .iter()
            .any(|start| text.starts_with(start))
            .then_some(text)
    }) {
        return message;
    }
    match kind(error) {
        Some(ErrorKind::NotFound) => {
            "We couldn't find that. Check the address and try again.".into()
        }
        Some(ErrorKind::Forbidden) => "You don't have permission to do that.".into(),
        Some(ErrorKind::LimitExceeded(_)) => "Slow down a little and try again.".into(),
        _ => "Something went wrong. Please try again.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_mentions_target_the_signed_in_user() {
        let user = <&UserId>::try_from("@me:example.org").unwrap();
        for (mentions, expected) in [
            (serde_json::json!({"user_ids":["@me:example.org"]}), true),
            (
                serde_json::json!({"user_ids":["@other:example.org"]}),
                false,
            ),
            (serde_json::json!({"room":true}), true),
            (serde_json::json!({}), false),
        ] {
            let content: RoomMessageEventContent = serde_json::from_value(serde_json::json!({
                "msgtype":"m.text", "body":"A message", "m.mentions":mentions
            }))
            .unwrap();
            assert_eq!(mentions_user(&content, Some(user)), expected);
        }
        let content = RoomMessageEventContent::text_plain("@me:example.org is plain text");
        assert!(!mentions_user(&content, Some(user)));
        assert!(!mentions_user(&content, None));
    }

    #[test]
    fn video_metadata_and_encrypted_thumbnail_survive_message_parsing() {
        let encrypted = serde_json::json!({
            "url":"mxc://example.org/thumbnail", "v":"v2",
            "key":{"kty":"oct","key_ops":["encrypt","decrypt"],"alg":"A256CTR","ext":true,
                "k":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
            "iv":"AAAAAAAAAAAAAAAAAAAAAA",
            "hashes":{"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}
        });
        let message: MessageType = serde_json::from_value(serde_json::json!({
            "msgtype":"m.video", "body":"A caption", "filename":"clip.mp4",
            "url":"mxc://example.org/video",
            "info":{"mimetype":"video/mp4","w":1920,"h":1080,"duration":12345,"thumbnail_file":encrypted}
        })).unwrap();
        let (_, caption, _, video) = describe(&message);
        let video = video.unwrap();
        assert_eq!((video.width, video.height), (1920, 1080));
        assert_eq!(video.duration, Some(Duration::from_millis(12345)));
        assert!(!caption.is_empty());
        assert!(matches!(video.thumbnail, Some(MediaSource::Encrypted(_))));
        assert_eq!(video.mime.as_deref(), Some("video/mp4"));
    }

    #[test]
    fn preserves_playable_media_sources_and_names() {
        for (kind, mime, name) in [
            ("m.video", "video/mp4", "clip.mp4"),
            ("m.audio", "audio/mpeg", "song.mp3"),
            ("m.file", "audio/flac", "song.flac"),
        ] {
            let msgtype: MessageType = serde_json::from_value(serde_json::json!({
                "msgtype": kind, "body": "A caption", "filename": name,
                "url": "mxc://example.org/media", "info": {"mimetype": mime}
            }))
            .unwrap();
            let (body, _, kind, media) = describe(&msgtype);
            assert_eq!(body, "A caption");
            assert!(kind == MessageKind::Attachment);
            let media = media.unwrap();
            assert_eq!(media.name, name);
            assert_eq!(media.mime.as_deref(), Some(mime));
            assert_eq!(media_url(&media.source), "mxc://example.org/media");
        }
    }

    #[test]
    fn strips_reply_fallbacks() {
        assert_eq!(without_reply("> <@a:b> hi\n> there\n\nhello"), "hello");
        assert_eq!(
            without_reply("> quote without break"),
            "> quote without break"
        );
        assert_eq!(without_reply("plain"), "plain");
    }

    #[test]
    fn picks_the_easiest_registration_flow() {
        let mut info = UiaaInfo::new(vec![
            uiaa::AuthFlow::new(vec![AuthType::ReCaptcha, AuthType::Terms]),
            uiaa::AuthFlow::new(vec![AuthType::RegistrationToken]),
        ]);
        assert_eq!(next_stage(&info), Some(AuthType::RegistrationToken));
        info.completed = vec![AuthType::RegistrationToken];
        assert_eq!(next_stage(&info), None);
    }
}
