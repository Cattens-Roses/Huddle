//! Shared chrome for message attachments and their media viewers.
use gpui::{AnyElement, App, Div, ElementId, FontWeight, Hsla, SharedString, div, prelude::*, px};
use ui::{ActiveTheme as _, Button, Text};

pub(super) fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1., ..color }
}

pub(super) fn card(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(theme.radius * 1.5)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .bg(opaque(theme.popover))
        .text_color(theme.popover_foreground)
}

pub(super) fn tool(
    id: impl Into<ElementId>,
    path: &'static str,
    label: impl Into<SharedString>,
) -> Button {
    Button::new(id)
        .ghost()
        .size(px(32.))
        .p_0()
        .flex_none()
        .icon(path)
        .tooltip(label)
}

pub(super) fn play(id: impl Into<ElementId>, path: &'static str, label: &'static str) -> Button {
    Button::new(id)
        .primary()
        .size(px(44.))
        .p_0()
        .flex_none()
        .rounded_full()
        .icon(path)
        .tooltip(label)
}

fn identity(name: &str, kind: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_2()
        .flex_1()
        .min_w_0()
        .child(super::icon(
            match kind {
                "Video" => "icons/video.svg",
                "Audio" => "icons/music.svg",
                _ => "icons/image.svg",
            },
            px(16.),
            theme.muted_foreground,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(theme.text(Text::Small))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name.to_owned()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(kind),
                ),
        )
}

pub(super) fn caption(name: &str, kind: &'static str, detail: String, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        .child(identity(name, kind, cx))
        .when(!detail.is_empty(), |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
        })
}

pub(super) fn header(name: &str, kind: &'static str, actions: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .h(px(64.))
        .px_4()
        .border_b_1()
        .border_color(theme.border)
        .child(identity(name, kind, cx))
        .child(div().flex().items_center().gap_1().children(actions))
}

pub(super) fn error(message: String, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .p_3()
        .rounded(theme.radius)
        .bg(theme.danger.opacity(0.1))
        .text_size(theme.text(Text::Small))
        .text_color(theme.danger)
        .child(message)
}
