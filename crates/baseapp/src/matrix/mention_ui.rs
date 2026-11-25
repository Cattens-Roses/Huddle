use super::*;
use gpui::Focusable as _;
use std::ops::Range;

pub(super) fn index(spot: Spot) -> usize {
    if spot == Spot::Main { 0 } else { 1 }
}

impl MatrixClient {
    fn mention_candidates(
        &self,
        spot: Spot,
        window: &Window,
        cx: &App,
    ) -> Option<(Range<usize>, Vec<Member>)> {
        let input = match spot {
            Spot::Main => &self.draft,
            Spot::Thread => &self.thread_draft,
        };
        let input = input.read(cx);
        if !input.focus_handle(cx).is_focused(window) {
            return None;
        }
        let selection = input.selection();
        let state = &self.mention_drafts[index(spot)];
        if state
            .dismissed
            .as_ref()
            .is_some_and(|(text, at)| text == input.text() && *at == selection.start)
        {
            return None;
        }
        let (range, query) = mentions::query(input.text(), selection)?;
        let query = query.to_lowercase();
        let members = self.members.get(self.room.as_ref()?)?;
        let mut matches = members
            .iter()
            .filter(|member| {
                member.name.to_lowercase().contains(&query)
                    || member.id.as_str().to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by_key(|member| {
            (
                !member.name.to_lowercase().starts_with(&query),
                member.name.to_lowercase(),
                member.id.clone(),
            )
        });
        matches.truncate(6);
        (!matches.is_empty()).then_some((range, matches))
    }

    fn insert_mention(
        &mut self,
        spot: Spot,
        member: Member,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((range, _)) = self.mention_candidates(spot, window, cx) else {
            return;
        };
        let input = match spot {
            Spot::Main => self.draft.clone(),
            Spot::Thread => self.thread_draft.clone(),
        };
        let label = format!("@{}", member.name.trim().replace(['\r', '\n'], " "));
        let old = input.read(cx).text().to_owned();
        self.mention_drafts[index(spot)].insert(&old, range.clone(), &label, member.id);
        input.update(cx, |input, cx| {
            input.insert_completion(range, &format!("{label} "), cx)
        });
        focus(&input, window, cx);
        cx.notify();
    }

    pub(super) fn mention_key(
        &mut self,
        spot: Spot,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.platform
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.shift
        {
            return;
        }
        let Some((_, candidates)) = self.mention_candidates(spot, window, cx) else {
            return;
        };
        let state = &mut self.mention_drafts[index(spot)];
        let count = candidates.len();
        let selected = state.selected.min(count - 1);
        match event.keystroke.key.as_str() {
            "up" => state.selected = (selected + count - 1) % count,
            "down" => state.selected = (selected + 1) % count,
            "enter" | "tab" => self.insert_mention(spot, candidates[selected].clone(), window, cx),
            "escape" => {
                let input = match spot {
                    Spot::Main => &self.draft,
                    Spot::Thread => &self.thread_draft,
                }
                .read(cx);
                state.dismissed = Some((input.text().to_owned(), input.selection().start));
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_mentions(
        &self,
        spot: Spot,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_, candidates) = self.mention_candidates(spot, window, cx)?;
        let theme = *cx.theme();
        let selected = self.mention_drafts[index(spot)]
            .selected
            .min(candidates.len() - 1);
        Some(
            div()
                .flex()
                .flex_col()
                .mb_2()
                .p_1()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child("Mention someone · ↑↓ select · Tab / Enter insert"),
                )
                .children(candidates.into_iter().enumerate().map(|(i, member)| {
                    div()
                        .id(SharedString::from(format!(
                            "mention-{}-{}",
                            index(spot),
                            member.id
                        )))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .when(i == selected, |row| row.bg(theme.secondary_hover))
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(self.face(
                            member.id.as_str(),
                            &member.name,
                            member.avatar.as_ref(),
                            px(30.),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(theme.text(Text::Small))
                                        .text_color(theme.foreground)
                                        .child(member.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(theme.muted_foreground)
                                        .child(member.id.to_string()),
                                ),
                        )
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.insert_mention(spot, member.clone(), window, cx)
                        }))
                }))
                .into_any_element(),
        )
    }
}
