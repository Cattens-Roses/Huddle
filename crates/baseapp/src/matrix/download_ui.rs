use super::*;
use crate::downloads::{Cancel, Progress};

pub(super) struct Job {
    pub ticket: u64,
    pub media: Picture,
    pub progress: Progress,
    pub cancel: Arc<Cancel>,
}

impl MatrixClient {
    pub(super) fn download_file(&mut self, media: Picture, cx: &mut Context<Self>) {
        if self
            .download_jobs
            .iter()
            .any(|job| job.media.key == media.key && job.progress.active())
        {
            return;
        }
        self.download_jobs.retain(|job| job.progress.active());
        self.download_ticket = self.download_ticket.wrapping_add(1);
        let ticket = self.download_ticket;
        let cancel = Arc::new(Cancel::default());
        self.download_jobs.push(Job {
            ticket,
            media: media.clone(),
            progress: Progress::Connecting,
            cancel: cancel.clone(),
        });
        self.command(Command::Download {
            ticket,
            media,
            directory: crate::downloads::directory(),
            cancel,
        });
        cx.notify();
    }

    pub(super) fn media_more(
        &self,
        media: &Picture,
        id: SharedString,
        cx: &mut Context<Self>,
    ) -> Button {
        let media = media.clone();
        media_ui::tool(id, "icons/ellipsis.svg", "File options")
            .disabled(media.key.starts_with("local:"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.media_menu = Some((media.clone(), window.mouse_position()));
                cx.notify();
            }))
    }

    pub(super) fn render_download_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (media, at) = self.media_menu.as_ref()?;
        let media = media.clone();
        let active = self
            .download_jobs
            .iter()
            .any(|job| job.media.key == media.key && job.progress.active());
        Some(
            deferred(
                ui::Popup::new(
                    *at,
                    ui::Menu::new("media-file-options").item(
                        ui::MenuItem::new(
                            "media-file-download",
                            if active { "Downloading…" } else { "Download" },
                        )
                        .icon("icons/download.svg")
                        .when(active, ui::MenuItem::disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.media_menu = None;
                            this.download_file(media.clone(), cx);
                        })),
                    ),
                )
                .on_close(cx.listener(|this, _, _, cx| {
                    this.media_menu = None;
                    cx.notify();
                })),
            )
            .into_any_element(),
        )
    }

    pub(super) fn render_file_download(
        &self,
        file: &Picture,
        key: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let file = file.clone();
        let active = self
            .download_jobs
            .iter()
            .any(|job| job.media.key == file.key && job.progress.active());
        media_ui::card(cx)
            .mt_1()
            .w(px(360.))
            .max_w_full()
            .p_3()
            .gap_3()
            .child(
                div()
                    .text_size(theme.text(Text::Small))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(file.name.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.muted_foreground)
                            .child(
                                file.size
                                    .map(crate::downloads::bytes)
                                    .unwrap_or_else(|| "File".into()),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{key}-download")))
                            .secondary()
                            .small()
                            .label(if active { "Downloading…" } else { "Download" })
                            .disabled(active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.download_file(file.clone(), cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_download_panel(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.download_jobs.is_empty() {
            return None;
        }
        let theme = *cx.theme();
        let viewport = window.viewport_size();
        let panel =
            media_ui::card(cx)
                .id("file-downloads")
                .p_3()
                .gap_3()
                .shadow_lg()
                .occlude()
                .w((viewport.width - px(32.)).min(px(360.)))
                .max_h((viewport.height - px(100.)).min(px(360.)))
                .overflow_y_scroll()
                .child(
                    div()
                        .text_size(theme.text(Text::Small))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Downloads"),
                )
                .children(self.download_jobs.iter().map(|job| {
                    let ticket = job.ticket;
                    let active = job.progress.active();
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(theme.text(Text::Small))
                                        .child(job.media.name.clone()),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "download-action-{ticket}"
                                    )))
                                    .ghost()
                                    .small()
                                    .label(if active { "Cancel" } else { "Dismiss" })
                                    .disabled(active && job.cancel.is_cancelled())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(job) = this
                                            .download_jobs
                                            .iter()
                                            .find(|job| job.ticket == ticket)
                                            && job.progress.active()
                                        {
                                            job.cancel.cancel();
                                        } else {
                                            this.download_jobs.retain(|job| job.ticket != ticket);
                                        }
                                        cx.notify();
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted_foreground)
                                .child(if active && job.cancel.is_cancelled() {
                                    "Cancelling…".into()
                                } else {
                                    job.progress.label()
                                }),
                        )
                        .when_some(job.progress.fraction(), |row, fraction| {
                            row.child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded_full()
                                    .bg(theme.border)
                                    .child(
                                        div()
                                            .w(gpui::relative(fraction))
                                            .h_full()
                                            .rounded_full()
                                            .bg(theme.primary),
                                    ),
                            )
                        })
                }));
        Some(
            deferred(
                anchored()
                    .position(point(viewport.width - px(16.), viewport.height - px(16.)))
                    .anchor(Anchor::BottomRight)
                    .child(panel),
            )
            .into_any_element(),
        )
    }
}
