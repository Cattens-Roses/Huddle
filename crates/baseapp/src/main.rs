#![recursion_limit = "256"]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, FontWeight, Hsla, Render, Rgba, ScrollHandle,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, point, px,
    svg,
};
use std::cell::Cell;
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use ui::{
    ActiveTheme as _, Button, Look, MenuItem, Motion, Motioned, Pace, Picker, Popovers, Rounding,
    Scrollbar, Scroller, Scrubber, ScrubberState, Stillness, Switch, Theme, ThemeKind,
    ThemeOverrides, WindowControls,
};

mod cache;
mod matrix;
mod notifications;

use notifications::NotificationPreferences;
mod presence;

use matrix::PhotoSize;

const FIRST_SIZE: gpui::Size<gpui::Pixels> = gpui::size(px(1240.), px(800.));
const AMBIENT_DOWNSCALE: f32 = 3.;
const AMBIENT_BLUR: f32 = 4.;
const AMBIENT_DISCS: usize = 18;
const AMBIENT_STYLE_FADE: std::time::Duration = std::time::Duration::from_millis(320);
const AMBIENT_SPECS: [(f32, f32, f32, f32, f32, f32, f32); 5] = [
    (0.2, 0.27, 1.0, 31., 0.4, 0.12, 0.1),
    (0.78, 0.22, 0.95, 38., 2.1, 0.1, 0.12),
    (0.52, 0.7, 1.15, 29., 3.6, 0.13, 0.09),
    (0.15, 0.76, 0.84, 43., 5.2, 0.1, 0.11),
    (0.85, 0.76, 0.9, 35., 2.7, 0.11, 0.1),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Appearance,
    Notifications,
    Cache,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AmbientStyle {
    Aurora,
    Sunset,
    Ocean,
    Forest,
    Rose,
}

impl AmbientStyle {
    const ALL: [Self; 5] = [
        Self::Aurora,
        Self::Sunset,
        Self::Ocean,
        Self::Forest,
        Self::Rose,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Aurora => "aurora",
            Self::Sunset => "sunset",
            Self::Ocean => "ocean",
            Self::Forest => "forest",
            Self::Rose => "rose",
        }
    }

    fn from_id(id: &str) -> Self {
        match id {
            "sunset" => Self::Sunset,
            "ocean" => Self::Ocean,
            "forest" => Self::Forest,
            "rose" => Self::Rose,
            _ => Self::Aurora,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Aurora => "Aurora",
            Self::Sunset => "Sunset",
            Self::Ocean => "Ocean",
            Self::Forest => "Forest",
            Self::Rose => "Rose",
        }
    }

    fn colors(self) -> (Hsla, Hsla) {
        let color = |h, s, l| Hsla { h, s, l, a: 1. };
        match self {
            Self::Aurora => (color(0.48, 0.78, 0.48), color(0.76, 0.72, 0.52)),
            Self::Sunset => (color(0.02, 0.84, 0.54), color(0.90, 0.78, 0.48)),
            Self::Ocean => (color(0.58, 0.82, 0.48), color(0.48, 0.74, 0.52)),
            Self::Forest => (color(0.36, 0.72, 0.42), color(0.48, 0.66, 0.48)),
            Self::Rose => (color(0.94, 0.76, 0.56), color(0.78, 0.72, 0.56)),
        }
    }

    fn blob_colors(self) -> [Hsla; 5] {
        let (primary, secondary) = self.colors();
        let base = Hsla { l: 0.39, ..primary };
        let second = Hsla {
            l: 0.38,
            ..secondary
        };
        [
            base,
            Hsla {
                l: 0.53,
                s: (base.s - 0.08).clamp(0.4, 0.78),
                ..base
            },
            second,
            Hsla {
                l: 0.27,
                s: (base.s + 0.04).clamp(0.45, 0.82),
                ..base
            },
            Hsla {
                h: (base.h + 0.97).rem_euclid(1.),
                l: 0.25,
                ..base
            },
        ]
    }
}

#[derive(Clone, Copy)]
struct AmbientTransition {
    from: [Hsla; 5],
    started: Instant,
}

struct BaseApp {
    page: Page,
    theme: ThemeKind,
    rounding: Rounding,
    blur: bool,
    blur_window: bool,
    font_size: f32,
    icon_pack: usize,
    sidebar_open: bool,
    transparent: bool,
    transparency: f32,
    opacity: ScrubberState,
    popovers: Popovers,
    stillness: Stillness,
    pace: Pace,
    ambient: bool,
    ambient_style: AmbientStyle,
    ambient_transition: Option<AmbientTransition>,
    ambient_motion: bool,
    photo_compress: bool,
    photo_size: PhotoSize,
    ambient_started: Instant,
    ambient_bounds: Rc<Cell<Bounds<gpui::Pixels>>>,
    focus: FocusHandle,
    scrollbar: Entity<Scrollbar>,
    notifications: NotificationPreferences,
    notifications_scrollbar: Entity<Scrollbar>,
    appearance_observer: Option<gpui::Subscription>,
    activation_observer: Option<gpui::Subscription>,
    matrix: Entity<matrix::MatrixClient>,
}

#[derive(Clone, Copy)]
struct Preferences {
    theme: ThemeKind,
    rounding: Rounding,
    blur: bool,
    blur_window: bool,
    font_size: f32,
    icon_pack: &'static str,
    transparent: bool,
    transparency: f32,
    stillness: Stillness,
    pace: Pace,
    ambient: bool,
    ambient_style: AmbientStyle,
    ambient_motion: bool,
    photo_compress: bool,
    photo_size: PhotoSize,
    notifications: NotificationPreferences,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: ThemeKind::System,
            rounding: Rounding::Subtle,
            blur: true,
            blur_window: false,
            font_size: 14.,
            icon_pack: icons::BASE,
            transparent: false,
            transparency: 0.,
            stillness: Stillness::System,
            pace: Pace::Base,
            ambient: false,
            ambient_style: AmbientStyle::Aurora,
            ambient_motion: true,
            photo_compress: true,
            photo_size: PhotoSize::Balanced,
            notifications: NotificationPreferences::default(),
        }
    }
}

impl Preferences {
    fn load() -> Self {
        let prefs = Self::default();
        let Some(path) = preferences_path() else {
            return prefs;
        };
        let Ok(contents) = std::fs::read_to_string(path) else {
            return prefs;
        };
        Self::parse(&contents)
    }

    fn parse(contents: &str) -> Self {
        let mut prefs = Self::default();
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "theme" => prefs.theme = ThemeKind::from_id(value),
                "rounding" => prefs.rounding = Rounding::from_id(value),
                "blur" => prefs.blur = value.parse().unwrap_or(prefs.blur),
                "blur_window" => prefs.blur_window = value.parse().unwrap_or(prefs.blur_window),
                "font_size" => {
                    prefs.font_size = value
                        .parse::<f32>()
                        .unwrap_or(prefs.font_size)
                        .clamp(ui::MIN_FONT, ui::MAX_FONT)
                }
                "icon_pack" if icons::pack(value).is_some() => {
                    prefs.icon_pack = icons::pack(value).unwrap().id
                }
                "transparent" => prefs.transparent = value.parse().unwrap_or(prefs.transparent),
                "transparency" => {
                    prefs.transparency = value
                        .parse::<f32>()
                        .unwrap_or(prefs.transparency)
                        .clamp(0., ui::MAX_TRANSPARENCY)
                }
                "stillness" => prefs.stillness = Stillness::from_id(value),
                "pace" => prefs.pace = Pace::from_id(value),
                "ambient" => prefs.ambient = value.parse().unwrap_or(prefs.ambient),
                "ambient_style" => prefs.ambient_style = AmbientStyle::from_id(value),
                "ambient_motion" => {
                    prefs.ambient_motion = value.parse().unwrap_or(prefs.ambient_motion)
                }
                "photo_compress" => {
                    prefs.photo_compress = value.parse().unwrap_or(prefs.photo_compress)
                }
                "photo_size" => prefs.photo_size = PhotoSize::from_id(value),
                "notifications" => {
                    prefs.notifications = serde_json::from_str(value).unwrap_or(prefs.notifications)
                }
                _ => {}
            }
        }
        prefs.transparent |= prefs.transparency > 0.;
        prefs
    }

    fn save(self) {
        let Some(path) = preferences_path() else {
            return;
        };
        let Some(parent) = path.parent() else { return };
        if let Err(error) =
            std::fs::create_dir_all(parent).and_then(|()| std::fs::write(path, self.encode()))
        {
            log::warn!("Could not save preferences: {error}");
        }
    }
    fn encode(self) -> String {
        format!(
            "theme={}\nrounding={}\nblur={}\nblur_window={}\nfont_size={}\nicon_pack={}\ntransparent={}\ntransparency={}\nstillness={}\npace={}\nambient={}\nambient_style={}\nambient_motion={}\nphoto_compress={}\nphoto_size={}\nnotifications={}\n",
            self.theme.id(),
            self.rounding.id(),
            self.blur,
            self.blur_window,
            self.font_size,
            self.icon_pack,
            self.transparent,
            self.transparency,
            self.stillness.id(),
            self.pace.id(),
            self.ambient,
            self.ambient_style.id(),
            self.ambient_motion,
            self.photo_compress,
            self.photo_size.id(),
            serde_json::to_string(&self.notifications).expect("notification preferences serialize")
        )
    }
}

fn preferences_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join("Library/Application Support");
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("baseapp").join("appearance.cfg"))
}

#[cfg(test)]
mod notification_preferences_tests {
    use super::*;

    #[test]
    fn notifications_round_trip_with_existing_preferences() {
        let prefs = Preferences {
            font_size: 18.,
            photo_compress: false,
            notifications: NotificationPreferences {
                enabled: false,
                direct_messages: true,
                room_messages: false,
                mentions: true,
                thread_replies: false,
                calls: true,
                show_preview: false,
                attention: false,
            },
            ..Default::default()
        };
        let restored = Preferences::parse(&prefs.encode());
        assert_eq!(restored.notifications, prefs.notifications);
        assert_eq!(restored.font_size, 18.);
        assert!(!restored.photo_compress);
    }

    #[test]
    fn notifications_keep_defaults_for_old_or_invalid_config() {
        for config in ["font_size=16\n", "notifications=not-json\nfont_size=16\n"] {
            let prefs = Preferences::parse(config);
            assert_eq!(prefs.notifications, NotificationPreferences::default());
            assert_eq!(prefs.font_size, 16.);
        }
    }
}

impl BaseApp {
    fn new(prefs: Preferences, cx: &mut Context<Self>) -> Self {
        let owner = cx.entity_id();
        Self {
            page: Page::Home,
            theme: prefs.theme,
            rounding: prefs.rounding,
            blur: prefs.blur,
            blur_window: prefs.blur_window,
            font_size: prefs.font_size,
            icon_pack: icons::packs()
                .position(|pack| pack.id == prefs.icon_pack)
                .unwrap_or_default(),
            sidebar_open: true,
            transparent: prefs.transparent,
            transparency: prefs.transparency,
            opacity: ScrubberState::new("appearance-opacity"),
            popovers: Popovers::default(),
            stillness: prefs.stillness,
            pace: prefs.pace,
            ambient: prefs.ambient,
            ambient_style: prefs.ambient_style,
            ambient_transition: None,
            ambient_motion: prefs.ambient_motion,
            photo_compress: prefs.photo_compress,
            photo_size: prefs.photo_size,
            ambient_started: Instant::now(),
            ambient_bounds: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
            scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(owner)),
            notifications: prefs.notifications,
            notifications_scrollbar: cx
                .new(|_| Scrollbar::new(ScrollHandle::new()).watching(owner)),
            appearance_observer: None,
            activation_observer: None,
            matrix: {
                let matrix = cx.new(|cx| {
                    let mut client = matrix::MatrixClient::new(cx);
                    client.set_photo_send(prefs.photo_compress, prefs.photo_size);
                    client.set_notifications(prefs.notifications);
                    client
                });
                cx.subscribe(&matrix, |this, _, _: &matrix::OpenSettings, cx| {
                    this.page = Page::Appearance;
                    this.matrix
                        .update(cx, |client, _| client.set_focused(false));
                    cx.notify();
                })
                .detach();
                cx.subscribe(&matrix, |_, _, _: &matrix::CacheChanged, cx| cx.notify())
                    .detach();
                matrix
            },
        }
    }

    fn look(&self) -> Look {
        Look {
            kind: self.theme,
            rounding: self.rounding,
            font: self.font_size,
            transparent: self.transparent,
            transparency: self.transparency,
            blur: self.blur,
            blur_window: self.blur_window,
            tint: self.ambient.then(|| self.ambient_style.colors().0),
            tint_secondary: self.ambient.then(|| self.ambient_style.colors().1),
        }
    }

    fn ambient_colors(&self, now: Instant) -> [Hsla; 5] {
        let target = self.ambient_style.blob_colors();
        let Some(transition) = self.ambient_transition else {
            return target;
        };
        let progress = (now.duration_since(transition.started).as_secs_f32()
            / AMBIENT_STYLE_FADE.as_secs_f32())
        .clamp(0., 1.);
        let progress = ui::ease_out_cubic(progress);
        std::array::from_fn(|index| ui::mix(transition.from[index], target[index], progress))
    }

    fn apply(&self, cx: &mut Context<Self>) {
        Theme::set(self.look(), &ThemeOverrides::default(), cx);
    }

    fn preferences(&self) -> Preferences {
        Preferences {
            theme: self.theme,
            rounding: self.rounding,
            blur: self.blur,
            blur_window: self.blur_window,
            font_size: self.font_size,
            icon_pack: icons::packs()
                .nth(self.icon_pack)
                .map_or(icons::BASE, |pack| pack.id),
            transparent: self.transparent,
            transparency: self.transparency,
            stillness: self.stillness,
            pace: self.pace,
            ambient: self.ambient,
            ambient_style: self.ambient_style,
            ambient_motion: self.ambient_motion,
            photo_compress: self.photo_compress,
            photo_size: self.photo_size,
            notifications: self.notifications,
        }
    }

    fn save(&self) {
        self.preferences().save();
    }

    fn remember_photo(&mut self, cx: &mut Context<Self>) {
        let compress = self.photo_compress;
        let size = self.photo_size;
        self.matrix
            .update(cx, |matrix, _| matrix.set_photo_send(compress, size));
        self.save();
        cx.notify();
    }

    fn settings_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        ui::TabBar::new("settings-sections").items(
            [
                (Page::Appearance, "settings-appearance", "Appearance"),
                (
                    Page::Notifications,
                    "settings-notifications",
                    "Notifications",
                ),
                (Page::Cache, "settings-cache", "Cache"),
            ]
            .into_iter()
            .map(|(page, id, label)| {
                Button::new(id)
                    .ghost()
                    .label(label)
                    .selected(self.page == page)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
            }),
        )
    }

    fn cache_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *cx.theme();
        let matrix = self.matrix.read(cx);
        let memory = matrix.cache_bytes() as f64 / (1024. * 1024.);
        let status = matrix.cache_status().map(str::to_owned);
        let available = matrix.can_clear_cache();
        let clear = Button::new("clear-media-cache")
            .secondary()
            .label("Clear media cache")
            .disabled(!available)
            .on_click(cx.listener(|this, _, _, cx| {
                this.matrix.update(cx, |matrix, cx| matrix.clear_cache(cx));
            }))
            .into_any_element();
        let rows = vec![
            self.row(
                "Downloaded media",
                "Reuse images, video, audio, and thumbnails instead of downloading them again",
                clear,
                cx,
            )
            .into_any_element(),
        ];
        Scroller::new("cache-settings", &self.notifications_scrollbar)
            .flex_1().min_h_0().min_w_0().flex_col().items_center()
            .child(div().flex().flex_col().gap_5().w_full().max_w(px(860.)).p_6()
                .child(self.settings_tabs(cx))
                .child(div().flex().flex_col().gap_2()
                    .child(div().text_size(theme.text(ui::Text::Title)).font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground).child("Cache"))
                    .child(div().text_color(theme.muted_foreground).child("Keep frequently used media close at hand.")))
                .child(self.settings_card("Media cache", rows, cx))
                .child(format!("Cached in memory: {memory:.1} MB"))
                .child(div().text_color(theme.muted_foreground).child("Disk retention limit: 512 MB. Media unused for 30 days is removed automatically."))
                .child(div().text_color(theme.muted_foreground).child("Clearing cached media keeps your messages, account, and pending uploads. Media can be downloaded again."))
                .when_some(status.clone(), |body, status| body.child(status))
                .when(!available && status.is_none(), |body| body.child("Sign in to manage the disk cache.")))
            .into_any_element()
    }

    fn notification_row(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        field: fn(&mut NotificationPreferences) -> &mut bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut prefs = self.notifications;
        let control = Switch::new(id, *field(&mut prefs))
            .disabled(!self.notifications.enabled && id != "notifications-enabled")
            .on_click(cx.listener(move |this, _, _, cx| {
                let value = field(&mut this.notifications);
                *value = !*value;
                let prefs = this.notifications;
                this.matrix
                    .update(cx, |matrix, _| matrix.set_notifications(prefs));
                this.save();
                cx.notify();
            }))
            .into_any_element();
        self.row(title, detail, control, cx).into_any_element()
    }

    fn notification_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = *cx.theme();
        let delivery = vec![self.notification_row(
            "notifications-enabled",
            "Desktop notifications",
            "Show alerts when you're not viewing the conversation",
            |p| &mut p.enabled,
            cx,
        )];
        let categories = vec![
            self.notification_row(
                "notifications-direct",
                "Direct messages",
                "Messages in one-to-one conversations",
                |p| &mut p.direct_messages,
                cx,
            ),
            self.notification_row(
                "notifications-rooms",
                "Room messages",
                "New messages in rooms",
                |p| &mut p.room_messages,
                cx,
            ),
            self.notification_row(
                "notifications-mentions",
                "Mentions",
                "Personal and @room mentions, even when other message alerts are off",
                |p| &mut p.mentions,
                cx,
            ),
            self.notification_row(
                "notifications-threads",
                "Thread replies",
                "Replies in message threads",
                |p| &mut p.thread_replies,
                cx,
            ),
            self.notification_row(
                "notifications-calls",
                "Incoming calls",
                "Desktop alerts for incoming voice and video calls",
                |p| &mut p.calls,
                cx,
            ),
        ];
        let presentation = vec![
            self.notification_row(
                "notifications-previews",
                "Message previews",
                "Include message text in desktop alerts",
                |p| &mut p.show_preview,
                cx,
            ),
            self.notification_row(
                "notifications-attention",
                "Taskbar attention",
                "Draw attention to the app when a desktop alert arrives",
                |p| &mut p.attention,
                cx,
            ),
        ];
        Scroller::new("notification-settings", &self.notifications_scrollbar)
            .flex_1().min_h_0().min_w_0().flex_col().items_center()
            .child(div().flex().flex_col().gap_5().w_full().max_w(px(860.)).p_6()
                .child(self.settings_tabs(cx))
                .child(div().flex().flex_col().gap_2()
                    .child(div().text_size(theme.text(ui::Text::Title)).font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground).child("Notifications"))
                    .child(div().text_color(theme.muted_foreground).child("Choose which desktop alerts you receive on this device.")))
                .child(self.settings_card("Delivery", delivery, cx))
                .child(self.settings_card("Notify me about", categories, cx))
                .child(self.settings_card("Privacy & attention", presentation, cx))
                .child(div().text_size(theme.text(ui::Text::Small)).text_color(theme.muted_foreground)
                    .child("These preferences apply to this device. Your system notification settings also apply.")))
            .into_any_element()
    }

    fn settings_card(
        &self,
        title: &'static str,
        content: Vec<gpui::AnyElement>,
        cx: &App,
    ) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .p_5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .pb_2()
                    .text_color(theme.foreground)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(content)
    }

    fn row(
        &self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        action: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = *cx.theme();
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap_4()
            .w_full()
            .py_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme.foreground).child(title.into()))
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .text_size(theme.text(ui::Text::Small))
                            .child(detail.into()),
                    ),
            )
            .child(div().flex_none().child(action))
    }
}

impl Render for BaseApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let sidebar_open = self.sidebar_open && self.page != Page::Home;
        let nav = |page, id: &'static str, icon: &'static str| {
            Button::new(id)
                .ghost()
                .icon(icons::path(icon))
                .tooltip(if page == Page::Home {
                    "nav-home"
                } else {
                    "nav-settings"
                })
                .size(px(44.))
                .rounded(px(12.))
                .when(
                    self.page == page || (page == Page::Appearance && self.page != Page::Home),
                    |button| {
                        button
                            .bg(theme.sidebar_accent)
                            .border_l_2()
                            .border_color(theme.primary)
                    },
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.page = page;
                    this.matrix.update(cx, |client, cx| {
                        client.set_focused(page == Page::Home && window.is_window_active());
                        if page == Page::Home {
                            client.dismiss_alert(cx);
                        }
                    });
                    cx.notify();
                }))
        };
        let sidebar = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .w(px(72.))
            .h_full()
            .py_4()
            .when(!theme.transparent && !self.ambient, |sidebar| {
                sidebar.bg(theme.sidebar)
            })
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                svg()
                    .path(icons::path("icons/baseapp.svg"))
                    .size(px(30.))
                    .text_color(theme.foreground),
            )
            .child(nav(Page::Home, "nav-home", "icons/house.svg"))
            .child(div().flex_1())
            .child(nav(Page::Appearance, "nav-settings", "icons/settings.svg"));

        let content = match self.page {
            Page::Home => self.matrix.clone().into_any_element(),
            Page::Notifications => self.notification_settings(cx),
            Page::Cache => self.cache_settings(cx),
            Page::Appearance => {
                let packs = icons::packs().collect::<Vec<_>>();
                let pack = packs.get(self.icon_pack % packs.len().max(1));
                let pack_label = pack.map_or_else(|| SharedString::from("Lucide"), |p| p.title());
                let theme_values = ThemeKind::ALL;
                let rounding_values = Rounding::ALL;
                let themes = div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .children(theme_values.into_iter().map(|kind| {
                        let preview = Theme::for_kind(kind);
                        div()
                            .id(format!("theme-preview-{}", kind.id()))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w(px(156.))
                            .p_3()
                            .rounded(theme.radius)
                            .border_2()
                            .border_color(if self.theme == kind {
                                theme.primary
                            } else {
                                theme.border
                            })
                            .cursor_pointer()
                            .hover(|style| style.border_color(theme.primary))
                            .child(
                                div()
                                    .flex()
                                    .h(px(64.))
                                    .rounded(px(6.))
                                    .overflow_hidden()
                                    .bg(preview.background)
                                    .child(div().w(px(18.)).h_full().bg(preview.sidebar))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .gap_2()
                                            .p_3()
                                            .child(
                                                div()
                                                    .h(px(6.))
                                                    .w(px(38.))
                                                    .rounded(px(2.))
                                                    .bg(preview.foreground),
                                            )
                                            .child(
                                                div()
                                                    .h(px(14.))
                                                    .w_full()
                                                    .rounded(px(3.))
                                                    .bg(preview.secondary),
                                            )
                                            .child(
                                                div()
                                                    .h(px(5.))
                                                    .w(px(28.))
                                                    .rounded(px(2.))
                                                    .bg(preview.primary),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_color(theme.foreground)
                                    .child(kind.label())
                                    .when(self.theme == kind, |label| label.child("•")),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.theme = kind;
                                this.apply(cx);
                                this.save();
                                cx.notify();
                            }))
                    }))
                    .into_any_element();
                let icons_picker = Picker::new("packs", &self.popovers, pack_label)
                    .width(Picker::REGULAR)
                    .items(packs.iter().enumerate().map(|(index, pack)| {
                        let id = pack.id;
                        MenuItem::new(id, pack.title())
                            .selected(index == self.icon_pack)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.icon_pack = index;
                                icons::set(id);
                                cx.refresh_windows();
                                this.save();
                                cx.notify();
                            }))
                    }))
                    .into_any_element();
                let corners = Picker::new("corners", &self.popovers, self.rounding.label())
                    .width(Picker::NARROW)
                    .items(rounding_values.into_iter().map(|rounding| {
                        MenuItem::new(rounding.id(), rounding.label())
                            .selected(rounding == self.rounding)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rounding = rounding;
                                this.apply(cx);
                                this.save();
                            }))
                    }))
                    .into_any_element();
                let blur = Switch::new("blur", self.blur)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.blur = !this.blur;
                        this.apply(cx);
                        this.save();
                    }))
                    .into_any_element();
                let blur_window = Switch::new("blur-window", self.blur_window && self.transparent)
                    .disabled(!self.transparent || !ui::WINDOW_BLUR)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.blur_window = !this.blur_window;
                        let blur_window = this.blur_window;
                        this.apply(cx);
                        window
                            .set_background_appearance(ui::backdrop(blur_window, this.transparent));
                        this.save();
                    }))
                    .into_any_element();
                let stillness = Picker::new("motion", &self.popovers, self.stillness.label())
                    .width(Picker::NARROW)
                    .items(Stillness::ALL.into_iter().map(|stillness| {
                        MenuItem::new(stillness.id(), stillness.label())
                            .selected(stillness == self.stillness)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stillness = stillness;
                                ui::motion::apply(this.stillness, this.pace, cx);
                                this.save();
                                cx.notify();
                            }))
                    }))
                    .into_any_element();
                let pace = Picker::new("pace", &self.popovers, self.pace.label())
                    .width(Picker::NARROW)
                    .items(Pace::ALL.into_iter().map(|pace| {
                        MenuItem::new(pace.id(), pace.label())
                            .selected(pace == self.pace)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pace = pace;
                                ui::motion::apply(this.stillness, this.pace, cx);
                                this.save();
                                cx.notify();
                            }))
                    }))
                    .into_any_element();
                let ambient = Switch::new("ambient", self.ambient)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ambient = !this.ambient;
                        this.apply(cx);
                        this.save();
                        cx.notify();
                    }))
                    .into_any_element();
                let ambient_style =
                    Picker::new("ambient-style", &self.popovers, self.ambient_style.label())
                        .width(Picker::REGULAR)
                        .items(AmbientStyle::ALL.into_iter().map(|style| {
                            MenuItem::new(style.id(), style.label())
                                .selected(style == self.ambient_style)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let now = Instant::now();
                                    let from = this.ambient_colors(now);
                                    this.ambient_style = style;
                                    this.ambient_transition = ui::motion::animates(cx)
                                        .then_some(AmbientTransition { from, started: now });
                                    if ui::motion::animates(cx) {
                                        Theme::fade(this.look(), &ThemeOverrides::default(), cx);
                                    } else {
                                        this.apply(cx);
                                    }
                                    this.save();
                                    cx.notify();
                                }))
                        }))
                        .into_any_element();
                let ambient_motion = Switch::new("ambient-motion", self.ambient_motion)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ambient_motion = !this.ambient_motion;
                        this.save();
                        cx.notify();
                    }))
                    .into_any_element();
                let photo_compress = Switch::new("photo-compress", self.photo_compress)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.photo_compress = !this.photo_compress;
                        this.remember_photo(cx);
                    }))
                    .into_any_element();
                let photo_size = Picker::new("photo-size", &self.popovers, self.photo_size.label())
                    .width(Picker::NARROW)
                    .items(PhotoSize::ALL.into_iter().map(|size| {
                        MenuItem::new(size.id(), size.label())
                            .selected(size == self.photo_size)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.photo_size = size;
                                this.remember_photo(cx);
                            }))
                    }))
                    .into_any_element();
                let font = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("font-smaller")
                            .small()
                            .outline()
                            .label("−")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.font_size = (this.font_size - 1.).max(ui::MIN_FONT);
                                this.apply(cx);
                                window.set_rem_size(cx.theme().font_size);
                                this.save();
                            })),
                    )
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(format!("{} px", self.font_size.round())),
                    )
                    .child(
                        Button::new("font-larger")
                            .small()
                            .outline()
                            .label("+")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.font_size = (this.font_size + 1.).min(ui::MAX_FONT);
                                this.apply(cx);
                                window.set_rem_size(cx.theme().font_size);
                                this.save();
                            })),
                    )
                    .into_any_element();
                let transparency = if self.transparent {
                    self.transparency
                } else {
                    0.
                };
                let value = 1. - transparency / ui::MAX_TRANSPARENCY;
                let opacity = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().w(theme.metrics.cover).child(
                            Scrubber::new(&self.opacity, value)
                                .colors(theme.progress_bar, theme.muted, theme.foreground)
                                .on_move(cx.listener(|this, fraction: &f32, window, cx| {
                                    this.transparency = (1. - *fraction) * ui::MAX_TRANSPARENCY;
                                    this.transparent = this.transparency > 0.;
                                    let transparent = this.transparent;
                                    let blur_window = this.blur_window;
                                    this.apply(cx);
                                    window.set_background_appearance(ui::backdrop(
                                        blur_window,
                                        transparent,
                                    ));
                                }))
                                .on_release(cx.listener(|this, _, _, _| this.save())),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(theme.metrics.control * 1.5)
                            .whitespace_nowrap()
                            .text_right()
                            .text_color(theme.foreground)
                            .child(format!("{}%", ((1. - transparency) * 100.).round())),
                    )
                    .into_any_element();

                Scroller::new("appearance-settings", &self.scrollbar)
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .w_full()
                            .max_w(px(860.))
                            .p_6()
                            .child(self.settings_tabs(cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(theme.text(ui::Text::Title))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child("Appearance"),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child("Make this space yours."),
                                    ),
                            )
                            .child(self.settings_card("Color & theme", vec![themes], cx))
                            .child(self.settings_card(
                                "Interface",
                                vec![
                                        self.row(
                                            "Icons",
                                            "Choose your icon family",
                                            icons_picker,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Corners",
                                            "Adjust the shape of controls",
                                            corners,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Text size",
                                            "Set a comfortable reading size",
                                            font,
                                            cx,
                                        )
                                        .into_any_element(),
                                    ],
                                cx,
                            ))
                            .child(self.settings_card(
                                "Transparency & ambient",
                                vec![
                                        self.row(
                                            "Opacity",
                                            "Control the window opacity",
                                            opacity,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Interface blur",
                                            "Soften translucent controls",
                                            blur,
                                            cx,
                                        )
                                        .into_any_element(),
                                        div()
                                            .when(ui::WINDOW_BLUR, |row| {
                                                row.child(self.row(
                                                    "Window blur",
                                                    "Blur the desktop behind the window",
                                                    blur_window,
                                                    cx,
                                                ))
                                            })
                                            .into_any_element(),
                                        self.row(
                                            "Ambient",
                                            "Show a soft color field behind the app",
                                            ambient,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Ambient style",
                                            "Choose the colors for the ambient field",
                                            ambient_style,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Ambient motion",
                                            "Let the color field drift slowly",
                                            ambient_motion,
                                            cx,
                                        )
                                        .into_any_element(),
                                    ],
                                cx,
                            ))
                            .child(self.settings_card(
                                "Motion",
                                vec![
                                        self.row(
                                            "Animations",
                                            "Choose how the interface moves",
                                            stillness,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row("Pace", "Set the animation speed", pace, cx)
                                            .into_any_element(),
                                    ],
                                cx,
                            ))
                            .child(self.settings_card(
                                "Sending photos",
                                vec![
                                        self.row(
                                            "Compress before sending",
                                            "Turn photos into smaller JPEGs first",
                                            photo_compress,
                                            cx,
                                        )
                                        .into_any_element(),
                                        self.row(
                                            "Photo size",
                                            "Longest side kept on a compressed photo",
                                            photo_size,
                                            cx,
                                        )
                                        .into_any_element(),
                                    ],
                                cx,
                            )),
                    )
                    .into_any_element()
            }
        };

        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(theme.metrics.title_bar)
            .px_2()
            .border_b_1()
            .border_color(theme.title_bar_border)
            .window_control_area(gpui::WindowControlArea::Drag)
            .when(self.page != Page::Home, |toolbar| {
                toolbar.child(
                    Button::new("sidebar-toggle")
                        .ghost()
                        .small()
                        .icon(icons::path(if self.sidebar_open {
                            "icons/panel-left-close.svg"
                        } else {
                            "icons/panel-left-open.svg"
                        }))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .px_3()
                    .text_color(theme.muted_foreground)
                    .text_size(theme.text(ui::Text::Small))
                    .child("Huddle")
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(WindowControls::new(false).on_close(|window, cx| {
                if presence::allow_close(window, cx) {
                    window.remove_window();
                }
            }));

        let ambient_animates = self.ambient && self.ambient_motion && ui::motion::animates(cx);
        let now = Instant::now();
        let ambient_transition_active = self.ambient_transition.is_some_and(|transition| {
            ui::motion::animates(cx) && now.duration_since(transition.started) < AMBIENT_STYLE_FADE
        });
        if self.ambient_transition.is_some() && !ambient_transition_active {
            self.ambient_transition = None;
        }
        let ambient_elapsed = if ambient_animates {
            now.duration_since(self.ambient_started).as_secs_f32()
        } else {
            0.
        };
        if ambient_animates || ambient_transition_active {
            window.request_animation_frame();
        }
        let bounds = self.ambient_bounds.get();
        let ambient_width = bounds.size.width.as_f32().max(1.) / AMBIENT_DOWNSCALE;
        let ambient_height = bounds.size.height.as_f32().max(1.) / AMBIENT_DOWNSCALE;
        let ambient_colors = self.ambient_colors(now);
        let ambient_bounds = self.ambient_bounds.clone();
        let ambient_opacity = if theme.transparent {
            theme.background.a
        } else {
            1.
        };
        div()
            .relative()
            .font_family(if cfg!(target_os = "windows") {
                "Segoe UI"
            } else if cfg!(target_os = "macos") {
                "Helvetica Neue"
            } else {
                "Noto Sans"
            })
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .track_focus(&self.focus)
            .when(self.ambient, |root| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .opacity(ambient_opacity)
                        .child(
                            canvas(
                                {
                                    let bounds = ambient_bounds.clone();
                                    move |measured, _, _| bounds.set(measured)
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .w(px(ambient_width))
                                .h(px(ambient_height))
                                .layer_scale(AMBIENT_DOWNSCALE)
                                .layer_scale_origin(point(0., 0.))
                                .blur(px(AMBIENT_BLUR / window.scale_factor()))
                                .children(AMBIENT_SPECS.into_iter().enumerate().map(
                                    |(index, spec)| {
                                        let (base_x, base_y, size, period, phase, amp_x, amp_y) =
                                            spec;
                                        let spin = TAU * ambient_elapsed / period;
                                        let x = base_x + amp_x * (spin + phase).sin();
                                        let y = base_y + amp_y * (spin * 0.83 + phase * 1.7).cos();
                                        let diameter = ambient_width.min(ambient_height) * size;
                                        let color = ambient_colors[index];
                                        div()
                                            .absolute()
                                            .left(px(x * ambient_width - diameter / 2.))
                                            .top(px(y * ambient_height - diameter / 2.))
                                            .size(px(diameter))
                                            .children((0..AMBIENT_DISCS).map(move |step| {
                                                let fraction = 1.
                                                    - step as f32 / AMBIENT_DISCS as f32
                                                        * (1. - 1. / AMBIENT_DISCS as f32);
                                                let opacity = 0.027
                                                    + step as f32 / (AMBIENT_DISCS as f32 - 1.)
                                                        * (0.079 - 0.027);
                                                let disc = diameter * fraction;
                                                div()
                                                    .absolute()
                                                    .left(px((diameter - disc) / 2.))
                                                    .top(px((diameter - disc) / 2.))
                                                    .size(px(disc))
                                                    .rounded_full()
                                                    .bg(shaded(color).opacity(opacity))
                                            }))
                                    },
                                )),
                        ),
                )
            })
            .child(toolbar)
            .child({
                let sidebar = div().flex_none().h_full().overflow_hidden().child(sidebar);
                let sidebar = if ui::motion::animates(cx) {
                    sidebar
                        .motion(
                            ("sidebar-width", usize::from(sidebar_open)),
                            Motion::Slow,
                            move |sidebar, progress| {
                                let width = if sidebar_open {
                                    72. * progress
                                } else {
                                    72. * (1. - progress)
                                };
                                sidebar.w(px(width.max(0.)))
                            },
                        )
                        .into_any_element()
                } else {
                    sidebar
                        .w(px(if sidebar_open { 72. } else { 0. }))
                        .into_any_element()
                };

                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar)
                    .child(content)
            })
    }
}

fn shaded(color: Hsla) -> Hsla {
    let rgba = Rgba::from(color);
    let kept = 0.45;
    Hsla::from(Rgba {
        r: rgba.r * kept,
        g: rgba.g * kept,
        b: rgba.b * kept,
        a: rgba.a,
    })
}

fn bind_input_keys(cx: &mut App) {
    use gpui::KeyBinding;
    use ui::{
        Backspace, BackspaceWord, Copy, Cut, Delete, DeleteWord, Down, End, Home, Left, Newline,
        Paste, Right, SelectAll, SelectDown, SelectEnd, SelectHome, SelectLeft, SelectRight,
        SelectUp, SelectWordLeft, SelectWordRight, Submit, Up, WordLeft, WordRight,
    };

    let context = Some(ui::INPUT_CONTEXT);
    let (command, word) = match cfg!(target_os = "macos") {
        true => ("cmd", "alt"),
        false => ("ctrl", "ctrl"),
    };
    let chord = |keys: &str| keys.replace("command", command).replace("word", word);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new(&chord("word-backspace"), BackspaceWord, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new(&chord("word-delete"), DeleteWord, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new(&chord("word-left"), WordLeft, context),
        KeyBinding::new(&chord("word-right"), WordRight, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new(&chord("word-shift-left"), SelectWordLeft, context),
        KeyBinding::new(&chord("word-shift-right"), SelectWordRight, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("shift-home", SelectHome, context),
        KeyBinding::new("shift-end", SelectEnd, context),
        KeyBinding::new(&chord("command-a"), SelectAll, context),
        KeyBinding::new(&chord("command-c"), Copy, context),
        KeyBinding::new(&chord("command-c"), Copy, None),
        KeyBinding::new(&chord("command-x"), Cut, context),
        KeyBinding::new(&chord("command-v"), Paste, context),
        KeyBinding::new("enter", Submit, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("escape", ui::Dismiss, context),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-left", Home, context),
            KeyBinding::new("cmd-right", End, context),
            KeyBinding::new("cmd-shift-left", SelectHome, context),
            KeyBinding::new("cmd-shift-right", SelectEnd, context),
        ]);
    }
    cx.bind_keys(bindings);
}

fn main() {
    env_logger::init();
    i18n::set(i18n::resolve("en"));
    let prefs = Preferences::load();
    icons::set(prefs.icon_pack);

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            let look = Look {
                kind: prefs.theme,
                rounding: prefs.rounding,
                font: prefs.font_size,
                transparent: prefs.transparent,
                transparency: prefs.transparency,
                blur: prefs.blur,
                blur_window: prefs.blur_window,
                tint: prefs.ambient.then(|| prefs.ambient_style.colors().0),
                tint_secondary: prefs.ambient.then(|| prefs.ambient_style.colors().1),
            };
            Theme::init(look, &ThemeOverrides::default(), cx);
            ui::motion::apply(prefs.stillness, prefs.pace, cx);
            bind_input_keys(cx);
            cx.set_app_identity("app.huddle.desktop", "Huddle");
            let bounds = WindowBounds::Windowed(Bounds::centered(None, FIRST_SIZE, cx));
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(bounds),
                        window_background: ui::backdrop(prefs.blur_window, prefs.transparent),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Huddle".into()),
                            appears_transparent: true,
                            ..Default::default()
                        }),
                        app_owns_titlebar_drag: true,
                        window_min_size: Some(gpui::size(px(480.), px(400.))),
                        app_id: Some("baseapp".into()),
                        ..Default::default()
                    },
                    {
                        move |window, cx| {
                            window.on_window_should_close(cx, presence::allow_close);
                            window.set_rem_size(cx.theme().font_size);
                            let view = cx.new(|cx| BaseApp::new(prefs, cx));
                            view.update(cx, |this, cx| {
                                this.appearance_observer = Some(cx.observe_window_appearance(
                                    window,
                                    |this, window, cx| {
                                        let reported = ThemeKind::reported(cx);
                                        let changed = ThemeKind::assumed() != Some(reported);
                                        if changed {
                                            ThemeKind::assume(reported);
                                        }
                                        if this.theme == ThemeKind::System && changed {
                                            Theme::fade(
                                                this.look(),
                                                &ThemeOverrides::default(),
                                                cx,
                                            );
                                        }
                                        window.set_background_appearance(ui::backdrop(
                                            this.blur_window,
                                            this.transparent,
                                        ));
                                    },
                                ));
                                this.matrix.update(cx, |client, _| {
                                    client.set_focused(window.is_window_active());
                                });
                                this.activation_observer = Some(cx.observe_window_activation(
                                    window,
                                    |this, window, cx| {
                                        let focused =
                                            window.is_window_active() && this.page == Page::Home;
                                        this.matrix.update(cx, |client, cx| {
                                            client.set_focused(focused);
                                            if focused {
                                                client.dismiss_alert(cx);
                                            }
                                        });
                                    },
                                ));
                            });
                            view
                        }
                    },
                )
                .expect("failed to open window");
            presence::install(window, cx);
            cx.activate(true);
        });
}
