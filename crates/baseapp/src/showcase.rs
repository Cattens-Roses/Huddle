use super::*;
use ui::{Modal, Toast};

pub(super) struct Showcase {
    scroll: Entity<Scrollbar>,
    enabled: bool,
    modal: bool,
    toast: Option<String>,
    selection: &'static str,
    amount: f32,
    slider: ScrubberState,
}

impl Showcase {
    pub(super) fn new(cx: &mut Context<BaseApp>) -> Self {
        let owner = cx.entity_id();
        Self {
            scroll: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(owner)),
            enabled: true,
            modal: false,
            toast: None,
            selection: "Balanced",
            amount: 0.65,
            slider: ScrubberState::new("demo-slider"),
        }
    }
}

impl BaseApp {
    pub(super) fn render_showcase(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let buttons = div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(
                Button::new("demo-primary")
                    .primary()
                    .label("Primary")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.demo.toast = Some("Primary action clicked".into());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("demo-secondary")
                    .secondary()
                    .label("Secondary")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.demo.toast = Some("Secondary action clicked".into());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("demo-outline")
                    .outline()
                    .label("Outline")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.demo.toast = Some("Outline action clicked".into());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("demo-disabled")
                    .primary()
                    .label("Disabled")
                    .disabled(true),
            );

        let menu = Picker::new("demo-menu", &self.popovers, self.demo.selection)
            .width(Picker::REGULAR)
            .items(
                ["Balanced", "Compact", "Comfortable"]
                    .into_iter()
                    .map(|choice| {
                        MenuItem::new(choice, choice)
                            .selected(self.demo.selection == choice)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.demo.selection = choice;
                                this.demo.toast = Some(format!("Selected: {choice}"));
                                cx.notify();
                            }))
                    }),
            )
            .into_any_element();
        let switch = Switch::new("demo-toggle", self.demo.enabled)
            .on_click(cx.listener(|this, _, _, cx| {
                this.demo.enabled = !this.demo.enabled;
                cx.notify();
            }))
            .into_any_element();
        let slider = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div().w(px(150.)).child(
                    Scrubber::new(&self.demo.slider, self.demo.amount)
                        .colors(theme.primary, theme.muted, theme.foreground)
                        .on_move(cx.listener(|this, value: &f32, _, cx| {
                            this.demo.amount = *value;
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .text_color(theme.foreground)
                    .w(px(44.))
                    .child(format!("{}%", (self.demo.amount * 100.).round())),
            )
            .into_any_element();

        // Sharp edges cross the panel, so the blurred and clear areas can be compared.
        let backdrop = div()
            .relative()
            .h(px(160.))
            .w_full()
            .overflow_hidden()
            .rounded(theme.radius)
            .bg(theme.secondary)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .gap_3()
                    .p_4()
                    .children((0..12).map(|index| {
                        div()
                            .flex_1()
                            .h_full()
                            .rounded(px(4.))
                            .bg(if index % 2 == 0 {
                                theme.primary
                            } else {
                                theme.danger
                            })
                    })),
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
                            .p_5()
                            .w(px(280.))
                            .max_w_full()
                            .h(px(96.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme
                                .popover
                                .opacity(if self.blur { ui::GLASS_FILL } else { 1. }))
                            .when(self.blur, |surface| surface.backdrop_blur(px(18.)))
                            .text_color(theme.foreground)
                            .child("Backdrop surface"),
                    ),
            );

        div().relative().flex().flex_col().flex_1().min_w_0().min_h_0()
            .child(Scroller::new("home-showcase", &self.demo.scroll)
                .flex_1().min_h_0().flex_col().items_center()
                .child(div().flex().flex_col().gap_5().w_full().max_w(px(860.)).p_6()
                    .child(div().flex().flex_col().gap_2()
                        .child(div().text_size(theme.text(ui::Text::Display)).font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground).child("hi"))
                        .child(div().text_color(theme.muted_foreground)
                            .child("Explore the building blocks of your app.")))
                    .child(self.settings_card("Buttons", vec![buttons.into_any_element()], cx))
                    .child(self.settings_card("Controls & menus", vec![
                        self.row("Menu", "Open the menu and choose an option", menu, cx).into_any_element(),
                        self.row("Switch", if self.demo.enabled { "Enabled" } else { "Disabled" }, switch, cx).into_any_element(),
                        self.row("Slider", "Drag to adjust the preview value", slider, cx).into_any_element(),
                    ], cx))
                    .child(self.settings_card("Backdrop", vec![
                        div().text_color(theme.muted_foreground).child(
                            if self.blur { "A frosted panel blurs the shapes behind it." }
                            else { "Enable Interface blur in Appearance to see the frosted effect." }
                        ).into_any_element(),
                        backdrop.into_any_element(),
                    ], cx))
                    .child(self.settings_card("Dialogs & notifications", vec![
                        div().flex().flex_wrap().gap_3()
                            .child(Button::new("demo-open-modal").primary().label("Open dialog")
                                .on_click(cx.listener(|this, _, _, cx| { this.demo.modal = true; cx.notify(); })))
                            .child(Button::new("demo-show-toast").outline().label("Show notification")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.demo.toast = Some("Your notification is ready".into()); cx.notify();
                                }))).into_any_element(),
                    ], cx))))
            .when_some(self.demo.toast.clone(), |root, message| root.child(
                div().absolute().bottom_4().right_4().max_w_full()
                    .child(Toast::new("demo-toast", message)
                        .on_dismiss(cx.listener(|this, _, _, cx| { this.demo.toast = None; cx.notify(); })))
            ))
            .when(self.demo.modal, |root| root.child(
                Modal::new("demo-modal", "Example dialog").close_button()
                    .detail("A focused panel with a backdrop behind it.")
                    .child(div().text_color(theme.foreground)
                        .child("Use dialogs for actions that need a little more attention."))
                    .on_dismiss(cx.listener(|this, _, _, cx| { this.demo.modal = false; cx.notify(); }))
                    .action(Button::new("demo-cancel").outline().label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| { this.demo.modal = false; cx.notify(); })))
                    .action(Button::new("demo-confirm").primary().label("Confirm")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.demo.modal = false; this.demo.toast = Some("Action confirmed".into()); cx.notify();
                        })))
            ))
    }
}
