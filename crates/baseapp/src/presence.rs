use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, Global, SharedString, SystemNotification, Task, Window, WindowHandle};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::BaseApp;

static QUITTING: AtomicBool = AtomicBool::new(false);

struct Seat {
    window: WindowHandle<BaseApp>,
    _icon: TrayIcon,
    _watch: Task<()>,
}

impl Global for Seat {}

/// Keeps a tray icon for the life of the process. Returns whether the icon is up, so closing
/// the window can stay in the tray instead of ending the process.
pub fn install(window: WindowHandle<BaseApp>, cx: &mut App) -> bool {
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
pub fn announce(cx: &mut App, title: &str, body: &str, room: &str) {
    cx.show_system_notification(SystemNotification {
        tag: SharedString::from(room.to_owned()),
        title: SharedString::from(title.to_owned()),
        body: SharedString::from(body.to_owned()),
        actions: Vec::new(),
    });
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
        window.update(cx, |_, window, _| reveal(window)).ok();
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

fn mark() -> Result<tray_icon::Icon, tray_icon::BadIcon> {
    const SIZE: u32 = 32;
    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
    let center = (SIZE as f32 - 1.) / 2.;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            if (dx * dx + dy * dy).sqrt() <= 14.5 {
                let index = ((y * SIZE + x) * 4) as usize;
                pixels[index] = 88;
                pixels[index + 1] = 101;
                pixels[index + 2] = 242;
                pixels[index + 3] = 255;
            }
        }
    }
    tray_icon::Icon::from_rgba(pixels, SIZE, SIZE)
}
