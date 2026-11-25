use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, CursorStyle, Element, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, LayoutId, MouseButton, PaintQuad, Pixels, Point, ShapedLine, Style,
    TextAlign, TextRun, UnderlineStyle, Window, WrappedLine, div, fill, point, px, relative, size,
    svg,
};

use i18n::t;

use crate::button::Button;
use crate::glass::{blurring, glass};
use crate::input::{
    CARET, CARET_LINES, Copy, Cut, INPUT_CONTEXT, Input, Paste, SelectAll, clamp_offset,
    clamp_range, masked_text,
};
use crate::menu::{Menu, MenuItem};
use crate::popup::Popup;
use crate::theme::ActiveTheme as _;

struct Text {
    input: Entity<Input>,
}

struct Painted {
    line: Option<ShapedLine>,
    lines: Vec<WrappedLine>,
    caret: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl IntoElement for Text {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Text {
    type RequestLayoutState = ();
    type PrepaintState = Painted;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let rows = match input.growing {
            true => input.rows.clamp(1, super::GROWING_ROWS),
            false => 1,
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = *cx.theme();
        let input = self.input.read(cx);
        if input.growing && !input.content.is_empty() {
            return prepaint_growing(&self.input, bounds, window, cx);
        }
        let empty = input.content.is_empty();
        let style = window.text_style();

        let secret = input.masked && !empty;
        let mask = match secret {
            true => Some(masked_text(&input.content)),
            false => None,
        };
        let text = match secret {
            true => mask
                .as_ref()
                .map(|(text, _)| text.clone())
                .unwrap_or_default(),
            false if empty => input.placeholder(),
            false => input.content.clone(),
        };
        let color = match empty {
            true => theme.muted_foreground,
            false => style.color,
        };

        let at = |offset: usize| match &mask {
            Some((_, map)) => map[offset.min(input.content.len())],
            None => offset,
        };
        let selected = match empty {
            true => 0..0,
            false => clamp_range(
                &text,
                &(at(input.selected_range.start)..at(input.selected_range.end)),
            ),
        };
        let cursor = match empty {
            true => 0,
            false => clamp_offset(&text, at(input.cursor())),
        };
        let marked = match empty {
            true => None,
            false => input
                .marked_range
                .as_ref()
                .map(|range| clamp_range(&text, &(at(range.start)..at(range.end)))),
        };

        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match marked {
            Some(marked) => [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);

        let (selection, caret) = match selected.is_empty() {
            true => (
                None,
                Some(fill(
                    Bounds::new(
                        point(
                            bounds.left() + line.x_for_index(cursor),
                            bounds.top() + (bounds.size.height - font_size * CARET_LINES) / 2.,
                        ),
                        size(CARET, font_size * CARET_LINES),
                    ),
                    theme.foreground,
                )),
            ),
            false => (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected.end),
                            bounds.bottom(),
                        ),
                    ),
                    theme.selection.opacity(0.4),
                )),
                None,
            ),
        };

        Painted {
            line: Some(line),
            lines: Vec::new(),
            caret,
            selection,
            selections: Vec::new(),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        painted: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let focused = focus.is_focused(window);
        if !painted.lines.is_empty() {
            if focused {
                for selection in painted.selections.drain(..) {
                    window.paint_quad(selection);
                }
            }
            let line_height = window.line_height();
            let scroll = self.input.read(cx).scroll;
            let mut y = bounds.top() - line_height * scroll as f32;
            let lines = std::mem::take(&mut painted.lines);
            for line in &lines {
                line.paint(
                    point(bounds.left(), y),
                    line_height,
                    TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                )
                .ok();
                y += line_height * (line.wrap_boundaries.len() + 1) as f32;
            }
            if focused && let Some(caret) = painted.caret.take() {
                window.paint_quad(caret);
            }
            self.input.update(cx, |input, _| {
                input.last_lines = lines;
                input.last_layout = None;
                input.last_bounds = Some(bounds);
            });
            return;
        }

        if focused && let Some(selection) = painted.selection.take() {
            window.paint_quad(selection);
        }

        let Some(line) = painted.line.take() else {
            return;
        };
        line.paint(
            bounds.origin,
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();

        if focused && let Some(caret) = painted.caret.take() {
            window.paint_quad(caret);
        }

        self.input.update(cx, |input, _| {
            input.last_layout = (!input.content.is_empty()).then_some(line);
            input.last_lines.clear();
            input.last_bounds = Some(bounds);
        });
    }
}

fn prepaint_growing(
    field: &Entity<Input>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Painted {
    let theme = *cx.theme();
    let input = field.read(cx);
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let text = input.content.clone();
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let marked = input.marked_range.clone().unwrap_or(0..0);
    let runs = [
        TextRun {
            len: marked.start,
            ..run.clone()
        },
        TextRun {
            len: marked.end - marked.start,
            underline: Some(UnderlineStyle {
                color: Some(run.color),
                thickness: px(1.),
                wavy: false,
            }),
            ..run.clone()
        },
        TextRun {
            len: text.len() - marked.end,
            ..run
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect::<Vec<_>>();
    let lines: Vec<WrappedLine> = window
        .text_system()
        .shape_text(
            text.clone(),
            font_size,
            &runs,
            Some(bounds.size.width),
            None,
        )
        .map(|lines| lines.into_iter().collect())
        .unwrap_or_default();
    let visual = lines
        .iter()
        .map(|line| line.wrap_boundaries.len() + 1)
        .sum::<usize>()
        .max(1);
    let cursor = input.cursor();
    let selected = input.selected_range.clone();
    let scroll = scrolled(input.scroll, visual, cursor, &lines, &text, line_height);
    let changed = input.rows != visual || input.scroll != scroll;
    field.update(cx, |input, cx| {
        input.line_height = line_height;
        input.rows = visual;
        input.scroll = scroll;
        if changed {
            cx.notify();
        }
    });

    let caret_h = font_size * super::CARET_LINES;
    let caret = point_of(cursor, &lines, &text, line_height).map(|origin| {
        fill(
            Bounds::new(
                point(
                    bounds.left() + origin.x,
                    bounds.top() + origin.y - line_height * scroll as f32
                        + (line_height - caret_h) / 2.,
                ),
                size(super::CARET, caret_h),
            ),
            theme.foreground,
        )
    });
    let selections = selection_quads(
        &lines,
        &text,
        &selected,
        bounds,
        line_height,
        scroll,
        theme.selection.opacity(0.4),
    );
    Painted {
        line: None,
        lines,
        caret,
        selection: None,
        selections,
    }
}

fn point_of(
    offset: usize,
    lines: &[WrappedLine],
    content: &str,
    line_height: Pixels,
) -> Option<Point<Pixels>> {
    if lines.is_empty() {
        return Some(point(px(0.), px(0.)));
    }
    let mut at = 0usize;
    let mut top = px(0.);
    for (index, line) in lines.iter().enumerate() {
        let len = line.len();
        let last = index + 1 == lines.len();
        if offset <= at + len || last {
            let local = offset.saturating_sub(at).min(len);
            let pos = line
                .position_for_index(local, line_height)
                .unwrap_or(point(px(0.), px(0.)));
            return Some(point(pos.x, top + pos.y));
        }
        at += len;
        if content[at..].starts_with('\n') {
            at += 1;
        }
        top += line_height * (line.wrap_boundaries.len() + 1) as f32;
    }
    None
}

fn scrolled(
    scroll: usize,
    visual: usize,
    cursor: usize,
    lines: &[WrappedLine],
    content: &str,
    line_height: Pixels,
) -> usize {
    let max_scroll = visual.saturating_sub(super::GROWING_ROWS);
    let mut scroll = scroll.min(max_scroll);
    let Some(origin) = point_of(cursor, lines, content, line_height) else {
        return scroll;
    };
    if line_height == px(0.) {
        return scroll;
    }
    let row = (origin.y / line_height) as usize;
    if row < scroll {
        scroll = row;
    } else if row >= scroll + super::GROWING_ROWS {
        scroll = row + 1 - super::GROWING_ROWS;
    }
    scroll.min(max_scroll)
}

fn selection_quads(
    lines: &[WrappedLine],
    content: &str,
    selected: &std::ops::Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    scroll: usize,
    color: gpui::Hsla,
) -> Vec<PaintQuad> {
    if selected.is_empty() {
        return Vec::new();
    }
    let mut quads = Vec::new();
    let mut at = 0usize;
    let mut top = px(0.);
    for line in lines {
        let len = line.len();
        let mut local_start = 0usize;
        let mut breaks = line
            .wrap_boundaries
            .iter()
            .map(|boundary| {
                line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
            })
            .collect::<Vec<_>>();
        breaks.push(len);
        for local_end in breaks {
            let start = at + local_start;
            let end = at + local_end;
            let from = selected.start.max(start);
            let to = selected.end.min(end);
            if from < to {
                // A wrap boundary is also the previous row's end. Resolve both
                // edges relative to this row, rather than a caret position that
                // can put the start at the previous row's right edge.
                let row_x = line.unwrapped_layout.x_for_index(local_start);
                let x = |offset: usize| line.unwrapped_layout.x_for_index(offset - at) - row_x;
                let y = bounds.top() + top - line_height * scroll as f32;
                quads.push(fill(
                    Bounds::from_corners(
                        point(bounds.left() + x(from), y),
                        point(bounds.left() + x(to), y + line_height),
                    ),
                    color,
                ));
            }
            local_start = local_end;
            top += line_height;
        }
        at += len;
        if content[at..].starts_with('\n') {
            at += 1;
        }
    }
    quads
}

impl Render for Input {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let line = window.line_height();
        let height = match self.growing {
            true => {
                let rows = self.rows.clamp(1, super::GROWING_ROWS);
                let text = line * rows as f32;
                // One line shares the pill with the icon and send button. Match
                // those controls so the line has equal space above and below.
                match rows == 1 {
                    true => text.max(theme.metrics.control_small + px(2.)),
                    false => text,
                }
            }
            false => match self.compact {
                true => theme.metrics.control_small,
                false => theme.metrics.field,
            },
        };
        let radius = match self.tucked {
            true => crate::tucked(theme.radius, window),
            false => theme.radius,
        };

        let empty = self.selected_range.is_empty();
        let paste = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some();
        let context_menu = self.context_menu.map(|position| {
            Popup::new(
                position,
                Menu::new("input-context-menu")
                    .item(
                        MenuItem::new("cut", t!("menu-cut"))
                            .icon("icons/scissors.svg")
                            .when(empty, MenuItem::disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cut(&Cut, window, cx);
                                this.context_menu = None;
                                cx.notify();
                            })),
                    )
                    .item(
                        MenuItem::new("copy", t!("menu-copy"))
                            .icon("icons/copy.svg")
                            .when(empty, MenuItem::disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.copy(&Copy, window, cx);
                                this.context_menu = None;
                                cx.notify();
                            })),
                    )
                    .item(
                        MenuItem::new("paste", t!("menu-paste"))
                            .icon("icons/clipboard-paste.svg")
                            .when(!paste, MenuItem::disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.paste(&Paste, window, cx);
                                this.context_menu = None;
                                cx.notify();
                            })),
                    )
                    .item(
                        MenuItem::new("select-all", t!("menu-select-all"))
                            .icon("icons/text-select.svg")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_all(&SelectAll, window, cx);
                                this.context_menu = None;
                                cx.notify();
                            })),
                    ),
            )
            .on_close(cx.listener(|this, _, _, cx| {
                this.context_menu = None;
                cx.notify();
            }))
        });

        div()
            .flex()
            .flex_1()
            .items_center()
            .gap_2()
            .min_w_0()
            .h(height)
            .px_3()
            .rounded(radius)
            .when_else(
                self.blurred && blurring(cx),
                |this| glass(this, cx),
                |this| this.bg(theme.secondary),
            )
            .when(self.blurred, |this| this.shadow_sm())
            .when_else(
                self.tucked,
                |this| this.w_full(),
                |this| this.border_1().border_color(theme.border),
            )
            .overflow_hidden()
            .key_context(INPUT_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::backspace_word))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word))
            .on_action(cx.listener(Self::backspace_to_start))
            .on_action(cx.listener(Self::delete_to_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::space))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when_some(self.icon.clone(), |this, path| {
                this.child(
                    svg()
                        .path(icons::path(path))
                        .size_4()
                        .flex_none()
                        .text_color(theme.muted_foreground),
                )
            })
            .child(Text {
                input: cx.entity().clone(),
            })
            .when(self.clearable && !self.content.is_empty(), |this| {
                this.child(
                    Button::new("input-clear")
                        .icon("icons/x.svg")
                        .tooltip("common-clear")
                        .aria_label(t!("common-clear"))
                        .small()
                        .ghost()
                        .px_1()
                        .on_click(cx.listener(|this, _, window, cx| this.clear(window, cx))),
                )
            })
            .when_some(context_menu, |this, menu| this.child(menu))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn wrapped(cx: &mut TestAppContext, text: &str, width: Pixels) -> Vec<WrappedLine> {
        let run = TextRun {
            len: text.len(),
            font: gpui::font("Arial"),
            color: gpui::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        cx.add_empty_window().update(|window, _| {
            window
                .text_system()
                .shape_text(text.to_owned().into(), px(14.), &[run], Some(width), None)
                .unwrap()
                .into_iter()
                .collect()
        })
    }

    #[gpui::test]
    fn select_all_highlights_every_wrapped_row_and_copies_the_entire_message(
        cx: &mut TestAppContext,
    ) {
        let text = "Wiadomość z polskimi znakami i emoji 🦀 zajmuje kilka zawiniętych linii w polu tekstowym.";
        let lines = wrapped(cx, text, px(120.));
        let rows = lines
            .iter()
            .map(|line| line.wrap_boundaries.len() + 1)
            .sum::<usize>();
        assert!(rows >= 3);
        let visual = cx.add_empty_window();
        let selected = visual.update(|window, cx| {
            let field = cx.new(|cx| {
                let mut field = Input::new("Message", cx).growing();
                field.set_text(text, cx);
                field
            });
            field.update(cx, |field, cx| {
                field.select_all(&SelectAll, window, cx);
                field.copy(&Copy, window, cx);
                field.selected_range.clone()
            })
        });
        assert_eq!(selected, 0..text.len());
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some(text)
        );
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(120.), px(200.)));
        let quads = selection_quads(&lines, text, &selected, bounds, px(20.), 0, gpui::white());
        assert_eq!(quads.len(), rows);
        for (row, quad) in quads.iter().enumerate() {
            assert_eq!(
                quad.bounds.left(),
                bounds.left(),
                "row {row} must start at the left edge"
            );
            assert!(
                quad.bounds.size.width > px(0.),
                "row {row} must have a visible highlight"
            );
            assert!(quad.bounds.size.width <= bounds.size.width);
            assert_eq!(quad.bounds.top(), bounds.top() + px(20.) * row as f32);
        }
    }

    #[gpui::test]
    fn selection_starting_at_a_wrap_boundary_handles_newlines_and_scroll(cx: &mut TestAppContext) {
        let text = "Pierwszy długi akapit wiadomości zawijany na kilka wierszy.\nDrugi akapit również ma więcej niż jeden wiersz.";
        let lines = wrapped(cx, text, px(120.));
        let boundary = lines[0].wrap_boundaries.first().unwrap();
        let start = lines[0].unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index;
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(120.), px(80.)));
        let quads = selection_quads(
            &lines,
            text,
            &(start..text.len()),
            bounds,
            px(20.),
            1,
            gpui::white(),
        );
        let rows = lines
            .iter()
            .map(|line| line.wrap_boundaries.len() + 1)
            .sum::<usize>();
        assert_eq!(quads.len(), rows - 1);
        assert_eq!(quads[0].bounds.origin, bounds.origin);
        for quad in quads {
            assert_eq!(quad.bounds.left(), bounds.left());
            assert!(quad.bounds.size.width > px(0.));
            assert!(quad.bounds.size.width <= bounds.size.width);
        }
    }
}
