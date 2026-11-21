//! Element Call, embedded in its own window.
//!
//! The page speaks the widget API through `postMessage`. Messages the page
//! sends are handed to the Matrix widget driver, and messages the driver
//! sends are posted back into the page. Echoes of those posts are ignored.

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId};
use matrix_sdk::widget::{
    Capabilities, CapabilitiesProvider, ClientProperties, EncryptionSystem, Intent,
    VirtualElementCallWidgetConfig, VirtualElementCallWidgetProperties, WidgetDriver,
    WidgetDriverHandle, WidgetSettings,
};
use matrix_sdk::{Client, RoomState};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use tokio::sync::{oneshot, watch};

use super::{Outbox, Update, post};

const CALL_URL: &str = "https://call.element.io";
const BRIDGE: &str = r#"
(() => {
  const recent = [];
  window.__huddlePost = (data) => {
    recent.push(JSON.stringify(data));
    window.postMessage(data, "*");
  };
  window.addEventListener("message", (event) => {
    const data = event.data;
    if (!data || (data.api !== "fromWidget" && data.api !== "toWidget")) return;
    const raw = JSON.stringify(data);
    const index = recent.indexOf(raw);
    if (index !== -1) {
      recent.splice(index, 1);
      return;
    }
    if (window.ipc) window.ipc.postMessage(raw);
  });
})();
"#;

enum Wake {
    Stop,
    Script(String),
}

struct Session {
    proxy: EventLoopProxy<Wake>,
    ended: watch::Sender<bool>,
}

impl Session {
    fn stop(&self) {
        self.ended.send(true).ok();
        self.proxy.send_event(Wake::Stop).ok();
    }
}

struct Slot {
    ticket: u64,
    live: bool,
    session: Option<Session>,
}

pub(super) struct Calls {
    slot: Mutex<Slot>,
}

impl Default for Calls {
    fn default() -> Self {
        Self {
            slot: Mutex::new(Slot {
                ticket: 0,
                live: false,
                session: None,
            }),
        }
    }
}

impl Calls {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Hangs up whatever is in progress and returns a ticket for the next call.
    pub(super) fn begin(&self) -> u64 {
        let mut slot = self.lock();
        slot.ticket += 1;
        slot.live = true;
        if let Some(session) = slot.session.take() {
            session.stop();
        }
        slot.ticket
    }

    pub(super) fn end(&self) {
        let mut slot = self.lock();
        slot.ticket += 1;
        slot.live = false;
        if let Some(session) = slot.session.take() {
            session.stop();
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.lock().live
    }

    /// Stores the window if this call is still the current one.
    /// The calling update is sent before the lock is released, so a hang up
    /// that arrives next is ordered after it.
    fn keep(&self, ticket: u64, session: Session, outbox: &Outbox, room: OwnedRoomId) -> bool {
        let mut slot = self.lock();
        if slot.ticket != ticket {
            drop(slot);
            session.stop();
            return false;
        }
        slot.session = Some(session);
        slot.live = true;
        post(outbox, Update::Calling { room });
        true
    }

    fn clear_if(&self, ticket: u64) -> bool {
        let mut slot = self.lock();
        if slot.ticket != ticket {
            return false;
        }
        slot.live = false;
        slot.session = None;
        true
    }
}

struct Grant;

impl CapabilitiesProvider for Grant {
    async fn acquire_capabilities(&self, capabilities: Capabilities) -> Capabilities {
        capabilities
    }
}

pub(super) struct Request {
    pub room: OwnedRoomId,
    pub direct: bool,
    pub voice: bool,
    pub join: bool,
}

pub(super) fn incoming(raw: &Raw<AnySyncTimelineEvent>) -> Option<OwnedEventId> {
    let kind = raw.get_field::<String>("type").ok().flatten()?;
    if !matches!(
        kind.as_str(),
        "m.rtc.notification"
            | "org.matrix.msc4075.rtc.notification"
            | "m.call.notify"
            | "org.matrix.msc4075.call.notify"
    ) {
        return None;
    }
    raw.get_field("event_id").ok().flatten()
}

pub(super) async fn place(
    client: &Client,
    outbox: &Outbox,
    calls: &Arc<Calls>,
    request: Request,
) -> Result<()> {
    let room = client
        .get_room(&request.room)
        .ok_or_else(|| anyhow!("The call couldn't find that room."))?;
    if room.state() != RoomState::Joined {
        anyhow::bail!("The call needs a room you've joined.");
    }

    let ticket = calls.begin();
    let result = open(outbox, calls, room, request, ticket).await;
    if result.is_err() && calls.clear_if(ticket) {
        post(outbox, Update::CallEnded);
    }
    result
}

async fn open(
    outbox: &Outbox,
    calls: &Arc<Calls>,
    room: matrix_sdk::Room,
    request: Request,
    ticket: u64,
) -> Result<()> {
    let encryption = match room.encryption_state().is_encrypted() {
        true => EncryptionSystem::PerParticipantKeys,
        false => EncryptionSystem::Unencrypted,
    };
    let widget_id = format!(
        "huddle{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_millis())
            .unwrap_or(0)
    );
    let props = VirtualElementCallWidgetProperties {
        element_call_url: CALL_URL.to_owned(),
        widget_id,
        encryption,
        ..VirtualElementCallWidgetProperties::default()
    };
    let config = VirtualElementCallWidgetConfig {
        intent: Some(intent(request.join, request.direct, request.voice)),
        confine_to_room: Some(true),
        app_prompt: Some(false),
        ..VirtualElementCallWidgetConfig::default()
    };
    let settings = WidgetSettings::new_virtual_element_call_widget(props, config)
        .map_err(|_| anyhow!("The call window couldn't open."))?;
    let url = settings
        .generate_webview_url(
            &room,
            ClientProperties::new("app.huddle.desktop", None, Some("dark".into())),
        )
        .await
        .map_err(|_| anyhow!("The call window couldn't open."))?;
    let extra = match request.join {
        true => "&header=none",
        false => "&header=none&sendNotificationType=ring",
    };
    let mut address = url.to_string();
    address.push_str(extra);

    let (driver, handle) = WidgetDriver::new(settings);
    let handle = Arc::new(handle);
    let bridge = handle.clone();
    let (ended_tx, mut ended_rx) = watch::channel(false);
    let ended_for_window = ended_tx.clone();
    let (ready_tx, ready_rx) = oneshot::channel();
    let on_close = {
        let calls = Arc::clone(calls);
        let outbox = outbox.clone();
        move || {
            if calls.clear_if(ticket) {
                post(&outbox, Update::CallEnded);
            }
        }
    };

    std::thread::Builder::new()
        .name("huddle-call".into())
        .spawn(move || window(address, bridge, ended_for_window, ready_tx, on_close))
        .map_err(|_| anyhow!("The call window couldn't open."))?;

    let proxy = ready_rx
        .await
        .map_err(|_| anyhow!("The call window couldn't open."))?
        .map_err(|error| {
            log::warn!("matrix: call window: {error}");
            anyhow!("The call window couldn't open.")
        })?;

    let scripts = proxy.clone();
    tokio::spawn(async move {
        while let Some(message) = handle.recv().await {
            if scripts.send_event(Wake::Script(message)).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        tokio::select! {
            result = driver.run(room, Grant) => {
                if result.is_err() {
                    log::warn!("matrix: the call widget stopped");
                }
            }
            _ = ended_rx.wait_for(|stop| *stop) => {}
        }
    });

    calls.keep(
        ticket,
        Session {
            proxy,
            ended: ended_tx,
        },
        outbox,
        request.room,
    );
    Ok(())
}

fn intent(join: bool, direct: bool, voice: bool) -> Intent {
    match (join, direct, voice) {
        (false, true, true) => Intent::StartCallDmVoice,
        (false, true, false) => Intent::StartCallDm,
        (true, true, true) => Intent::JoinExistingDmVoice,
        (true, true, false) => Intent::JoinExistingDm,
        (true, false, _) => Intent::JoinExisting,
        (false, false, _) => Intent::StartCall,
    }
}

fn window(
    url: String,
    bridge: Arc<WidgetDriverHandle>,
    ended: watch::Sender<bool>,
    ready: oneshot::Sender<Result<EventLoopProxy<Wake>, String>>,
    on_close: impl FnOnce() + Send + 'static,
) {
    let mut builder = EventLoopBuilder::<Wake>::with_user_event();
    let event_loop = builder.build();
    let proxy = event_loop.create_proxy();
    let view = WindowBuilder::new()
        .with_title("Huddle call")
        .with_inner_size(LogicalSize::new(1100.0, 720.0))
        .build(&event_loop);
    let window = match view {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return;
        }
    };
    let webview = match build_view(&window, url, bridge) {
        Ok(webview) => webview,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(proxy)).is_err() {
        return;
    }
    let mut closed = false;
    let mut on_close = Some(on_close);
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _alive = &window;
        match event {
            Event::UserEvent(Wake::Stop)
            | Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if !closed {
                    closed = true;
                    ended.send(true).ok();
                    if let Some(on_close) = on_close.take() {
                        on_close();
                    }
                }
                *flow = ControlFlow::Exit;
            }
            Event::UserEvent(Wake::Script(message)) => {
                let script = format!("window.__huddlePost({message});");
                webview.evaluate_script(&script).ok();
            }
            _ => {}
        }
    });
}

fn build_view(
    window: &tao::window::Window,
    url: String,
    bridge: Arc<WidgetDriverHandle>,
) -> Result<wry::WebView, String> {
    let builder = wry::WebViewBuilder::new()
        .with_url(&url)
        .with_autoplay(true)
        .with_initialization_script(BRIDGE)
        .with_ipc_handler(move |request| {
            bridge.send(request.into_body());
        })
        .with_permission_handler(|kind| match kind {
            wry::PermissionKind::Microphone
            | wry::PermissionKind::Camera
            | wry::PermissionKind::DisplayCapture => wry::PermissionResponse::Allow,
            _ => wry::PermissionResponse::Default,
        });

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    {
        builder.build(window).map_err(|error| error.to_string())
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| "the call window has no container".to_owned())?;
        builder.build_gtk(vbox).map_err(|error| error.to_string())
    }
}
