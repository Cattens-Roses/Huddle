use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, Global, SharedString, SystemNotification, Task, Window, WindowHandle};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::Huddle;

static QUITTING: AtomicBool = AtomicBool::new(false);

struct Seat {
    window: WindowHandle<Huddle>,
    _icon: TrayIcon,
    _watch: Task<()>,
}

impl Global for Seat {}

/// Keeps a tray icon for the life of the process. Returns whether the icon is up, so closing
/// the window can stay in the tray instead of ending the process.
pub fn install(window: WindowHandle<Huddle>, cx: &mut App) -> bool {
    let Ok(icon) = mark() else {
        log::warn!("tray: the icon could not be built");
        return false;
    };
    let menu = Menu::new();
    let open = MenuItem::with_id("huddle-open", "Open Huddle", true, None);
    let quit = MenuItem::with_id("huddle-quit", "Quit Huddle", true, None);
    if menu.append_items(&[&open, &quit]).is_err() {
        log::warn!("tray: the menu could not be built");
        return false;
    }
    let Ok(icon) = TrayIconBuilder::new()
        .with_tooltip("Huddle")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
    else {
        log::warn!("tray: the icon could not be placed");
        return false;
    };

    cx.on_system_notification_response(move |response, cx| {
        let Some(window) = cx.try_global::<Seat>().map(|seat| seat.window) else {
            return;
        };
        let room = response.tag.to_string();
        window
            .update(cx, |app, window, cx| {
                app.set_in_tray(false, window, cx);
                reveal(window);
                app.matrix
                    .update(cx, |client, cx| client.focus_room(&room, window, cx));
            })
            .ok();
    });

    let watch = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            cx.update(drain);
        }
    });
    cx.set_global(Seat {
        window,
        _icon: icon,
        _watch: watch,
    });
    true
}

/// A new message the person is not already looking at.
pub fn announce(cx: &mut App, title: &str, body: &str, room: &str, attention: bool) {
    cx.show_system_notification(SystemNotification {
        tag: SharedString::from(room.to_owned()),
        title: SharedString::from(title.to_owned()),
        body: SharedString::from(body.to_owned()),
        actions: Vec::new(),
    });
    if !attention {
        return;
    }
    let Some(window) = cx.try_global::<Seat>().map(|seat| seat.window) else {
        return;
    };
    window
        .update(cx, |_, window, _| window.request_attention())
        .ok();
}

/// Closing the window hides it while the tray icon can bring it back. Quit still closes it.
pub fn allow_close(window: &mut Window, cx: &mut App) -> bool {
    if QUITTING.load(Ordering::Relaxed) {
        return true;
    }
    if cx.try_global::<Seat>().is_some() {
        conceal(window);
        // Close handlers can run while Huddle is already borrowed. Defer the
        // entity update, then replace the retained frame even though it is hidden.
        cx.defer(|cx| {
            let Some(handle) = cx.try_global::<Seat>().map(|seat| seat.window) else {
                return;
            };
            handle
                .update(cx, |app, window, cx| {
                    app.set_in_tray(true, window, cx);
                    window.refresh();
                })
                .ok();
            gpui::AnyWindowHandle::from(handle)
                .update(cx, |_, window, cx| {
                    window.draw(cx).clear(cx);
                })
                .ok();
            cx.spawn(async move |cx| {
                // Give decoder workers time to observe their stop flag and drop buffers.
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let hidden = handle.update(cx, |app, _, _| app.in_tray).unwrap_or(false);
                if hidden {
                    cx.background_executor()
                        .spawn(async {
                            release_working_set();
                        })
                        .detach();
                }
            })
            .detach();
        });
    } else {
        // Keep the window accessible from the taskbar if tray installation failed.
        window.minimize_window();
    }
    false
}

fn drain(cx: &mut App) {
    let Some(window) = cx.try_global::<Seat>().map(|seat| seat.window) else {
        return;
    };
    let mut open = false;
    let mut quit = false;
    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => open = true,
            _ => {}
        }
    }
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        match event.id.0.as_str() {
            "huddle-open" => open = true,
            "huddle-quit" => quit = true,
            _ => {}
        }
    }
    if quit {
        QUITTING.store(true, Ordering::Relaxed);
        cx.quit();
        return;
    }
    if open {
        window
            .update(cx, |app, window, cx| {
                app.set_in_tray(false, window, cx);
                reveal(window);
            })
            .ok();
    }
}

/// Return unused resident pages after releasing our references. This does not
/// destroy session state or trim repeatedly while the window is being used.
fn release_working_set() {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::{
            ProcessStatus::EmptyWorkingSet, Threading::GetCurrentProcess,
        };
        if EmptyWorkingSet(GetCurrentProcess()) == 0 {
            log::debug!(
                "tray: working set trim failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

fn reveal(window: &mut Window) {
    place(window, true);
    window.activate_window();
}

fn conceal(window: &mut Window) {
    if !place(window, false) {
        window.minimize_window();
    }
}

fn place(window: &Window, shown: bool) -> bool {
    #[cfg(target_os = "windows")]
    {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return false;
        };
        let command = if shown {
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW
        } else {
            windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE
        };
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                handle.hwnd.get() as windows_sys::Win32::Foundation::HWND,
                command,
            );
        }
        return true;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, shown);
        false
    }
}

fn mark() -> anyhow::Result<tray_icon::Icon> {
    let image = image::load_from_memory(include_bytes!("../../../assets/tray/huddle.png"))?
        .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    Ok(tray_icon::Icon::from_rgba(image.into_raw(), 32, 32)?)
}
