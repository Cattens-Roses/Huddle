//! A document selection shared by all rich-text blocks in a conversation.
use gpui::{Bounds, Pixels, Point, TextLayout};
use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Default)]
pub struct Selection(Rc<RefCell<State>>);
#[derive(Default)]
struct State {
    context: String,
    document: Vec<(String, String)>,
    layouts: HashMap<String, (TextLayout, Bounds<Pixels>)>,
    anchor: Option<Endpoint>,
    focus: Option<Endpoint>,
    unit: Unit,
    dragging: bool,
    moved: bool,
    travelled: bool,
    origin: Option<Point<Pixels>>,
}
#[derive(Clone, PartialEq, Eq)]
struct Endpoint {
    id: String,
    range: Range<usize>,
}
#[derive(Clone, Copy, Default)]
enum Unit {
    #[default]
    Character,
    Word,
    Paragraph,
}

impl Selection {
    pub fn prepare(&self, context: String, document: Vec<(String, &str)>) {
        let mut state = self.0.borrow_mut();
        if state.context != context {
            *state = State {
                context,
                ..State::default()
            };
        }
        state.layouts.clear();
        if state.document.len() != document.len()
            || state
                .document
                .iter()
                .zip(&document)
                .any(|(a, b)| a.0 != b.0 || a.1 != b.1)
        {
            let changed_endpoint = [&state.anchor, &state.focus].iter().any(|point| {
                point.as_ref().is_some_and(|point| {
                    let old = state
                        .document
                        .iter()
                        .find(|(id, _)| id == &point.id)
                        .map(|(_, text)| text.as_str());
                    let new = document
                        .iter()
                        .find(|(id, _)| id == &point.id)
                        .map(|(_, text)| *text);
                    old != new
                })
            });
            state.document = document
                .into_iter()
                .map(|(id, text)| (id, text.to_owned()))
                .collect();
            if changed_endpoint
                || [&state.anchor, &state.focus].iter().any(|point| {
                    point.as_ref().is_some_and(|point| {
                        !state
                            .document
                            .iter()
                            .any(|(id, text)| id == &point.id && point.range.end <= text.len())
                    })
                })
            {
                state.anchor = None;
                state.focus = None;
                state.dragging = false;
            }
        }
    }
    pub fn register(&self, id: String, layout: TextLayout, bounds: Bounds<Pixels>) {
        self.0.borrow_mut().layouts.insert(id, (layout, bounds));
    }
    pub fn clear(&self) -> bool {
        let mut state = self.0.borrow_mut();
        let changed = state.anchor.take().is_some();
        state.focus = None;
        state.dragging = false;
        changed
    }
    pub fn dragging(&self) -> bool {
        self.0.borrow().dragging
    }
    pub fn autoscrolling(&self) -> bool {
        let state = self.0.borrow();
        state.dragging && state.travelled
    }
    pub fn thread(&self) -> bool {
        self.0
            .borrow()
            .anchor
            .as_ref()
            .is_some_and(|point| point.id.starts_with("t-"))
    }
    pub fn begin(
        &self,
        id: &str,
        index: usize,
        clicks: usize,
        shift: bool,
        position: Point<Pixels>,
    ) {
        let mut state = self.0.borrow_mut();
        let Some((_, text)) = state.document.iter().find(|(key, _)| key == id) else {
            return;
        };
        let unit = if clicks >= 3 {
            Unit::Paragraph
        } else if clicks == 2 {
            Unit::Word
        } else {
            Unit::Character
        };
        let point = Endpoint {
            id: id.into(),
            range: span(text, index, unit),
        };
        if !shift
            || state
                .anchor
                .as_ref()
                .is_none_or(|anchor| scope(&anchor.id) != scope(id))
        {
            state.anchor = Some(point.clone());
        }
        state.focus = Some(point);
        state.unit = unit;
        state.dragging = true;
        state.moved = shift || clicks != 1;
        state.travelled = false;
        state.origin = Some(position);
    }
    pub fn extend(&self, position: Point<Pixels>) -> bool {
        let mut state = self.0.borrow_mut();
        if !state.dragging {
            return false;
        }
        let near_origin = state.origin.is_some_and(|start| {
            (position.x - start.x).abs() < gpui::px(3.)
                && (position.y - start.y).abs() < gpui::px(3.)
        });
        if !state.moved && near_origin {
            return false;
        }
        state.travelled |= !near_origin;
        state.moved = true;
        let Some(anchor) = &state.anchor else {
            return false;
        };
        // Choose the closest visible text block in the original pane. This also
        // covers margins and the gaps between paragraphs and messages.
        let target = state
            .document
            .iter()
            .filter(|(id, _)| scope(id) == scope(&anchor.id))
            .filter_map(|(id, text)| {
                state
                    .layouts
                    .get(id)
                    .map(|(layout, bounds)| (id, text, layout, bounds))
            })
            .min_by(|a, b| distance(position, a.3).total_cmp(&distance(position, b.3)));
        let Some((id, text, layout, bounds)) = target else {
            return false;
        };
        let index = if matches!(state.unit, Unit::Character) {
            super::rich::index_at_bounds(layout, bounds, position, text)
        } else {
            match layout.index_for_position(position) {
                Ok(index) | Err(index) => super::rich::boundary(text, index),
            }
        };
        let point = Endpoint {
            id: id.clone(),
            range: span(text, index, state.unit),
        };
        if state.focus.as_ref() == Some(&point) {
            return false;
        }
        state.focus = Some(point);
        true
    }
    pub fn finish(&self) -> bool {
        let mut state = self.0.borrow_mut();
        let click = state.dragging && !state.moved;
        state.dragging = false;
        click
    }
    pub fn click_index(&self, id: &str) -> Option<usize> {
        let state = self.0.borrow();
        state
            .anchor
            .as_ref()
            .filter(|anchor| anchor.id == id)
            .map(|anchor| anchor.range.start)
    }
    pub fn select_all(&self) {
        let mut state = self.0.borrow_mut();
        let pane = state
            .anchor
            .as_ref()
            .map_or("m", |anchor| scope(&anchor.id))
            .to_owned();
        let first = state.document.iter().find(|(id, _)| scope(id) == pane);
        let last = state
            .document
            .iter()
            .rev()
            .find(|(id, _)| scope(id) == pane);
        let points = first.zip(last).map(|((first, _), (last, text))| {
            (
                Endpoint {
                    id: first.clone(),
                    range: 0..0,
                },
                Endpoint {
                    id: last.clone(),
                    range: text.len()..text.len(),
                },
            )
        });
        if let Some((anchor, focus)) = points {
            state.anchor = Some(anchor);
            state.focus = Some(focus);
            state.dragging = false;
        }
    }
    pub fn range(&self, id: &str) -> Option<Range<usize>> {
        let state = self.0.borrow();
        ranges(&state)
            .into_iter()
            .find_map(|(key, range)| (key == id).then_some(range))
    }
    pub fn text(&self) -> String {
        let state = self.0.borrow();
        ranges(&state)
            .into_iter()
            .filter_map(|(id, range)| {
                state
                    .document
                    .iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, text)| text[range].to_owned())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
fn scope(id: &str) -> &str {
    id.split_once('-').map_or(id, |(scope, _)| scope)
}
fn distance(point: Point<Pixels>, bounds: &Bounds<Pixels>) -> f32 {
    let dy = if point.y < bounds.top() {
        bounds.top() - point.y
    } else if point.y > bounds.bottom() {
        point.y - bounds.bottom()
    } else {
        gpui::px(0.)
    };
    let dx = if point.x < bounds.left() {
        bounds.left() - point.x
    } else if point.x > bounds.right() {
        point.x - bounds.right()
    } else {
        gpui::px(0.)
    };
    f32::from(dy) * 10000. + f32::from(dx)
}
fn ranges(state: &State) -> Vec<(&str, Range<usize>)> {
    let (Some(anchor), Some(focus)) = (&state.anchor, &state.focus) else {
        return Vec::new();
    };
    let Some(a) = state.document.iter().position(|(id, _)| id == &anchor.id) else {
        return Vec::new();
    };
    let Some(b) = state.document.iter().position(|(id, _)| id == &focus.id) else {
        return Vec::new();
    };
    let (start, end, first, last) = if (a, anchor.range.start) <= (b, focus.range.start) {
        (a, b, anchor, focus)
    } else {
        (b, a, focus, anchor)
    };
    state
        .document
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start + 1)
        .filter(|(_, (id, _))| scope(id) == scope(&anchor.id))
        .filter_map(|(i, (id, text))| {
            let from = if i == start { first.range.start } else { 0 };
            let to = if i == end { last.range.end } else { text.len() };
            (from < to).then_some((id.as_str(), from..to))
        })
        .collect()
}
fn span(text: &str, index: usize, unit: Unit) -> Range<usize> {
    let index = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .take_while(|i| *i <= index)
        .last()
        .unwrap_or(0);
    match unit {
        Unit::Character => index..index,
        Unit::Word => text
            .split_word_bound_indices()
            .find(|(i, word)| {
                index < i + word.len() || (index == text.len() && i + word.len() == index)
            })
            .map(|(i, word)| i..i + word.len())
            .unwrap_or(text.len()..text.len()),
        Unit::Paragraph => {
            let start = text[..index].rfind('\n').map_or(0, |i| i + 1);
            let end = text[index..]
                .find('\n')
                .map_or(text.len(), |i| index + i + 1);
            start..end
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};
    fn selection() -> Selection {
        let selection = Selection::default();
        selection.prepare(
            "room".into(),
            vec![
                ("m-one".into(), "Hello world"),
                ("m-code".into(), "let x = 1;"),
                ("m-two".into(), "Second message"),
                ("t-one".into(), "Thread reply"),
            ],
        );
        selection
    }
    fn click(selection: &Selection, id: &str, index: usize, clicks: usize, shift: bool) {
        selection.begin(id, index, clicks, shift, point(px(0.), px(0.)));
        selection.finish();
    }
    #[test]
    fn selection_crosses_blocks_and_messages_in_both_directions() {
        let selection = selection();
        click(&selection, "m-one", 6, 1, false);
        click(&selection, "m-two", 6, 1, true);
        assert_eq!(selection.text(), "world\nlet x = 1;\nSecond");
        assert_eq!(selection.range("m-one"), Some(6..11));
        assert_eq!(selection.range("m-code"), Some(0..10));
        assert!(selection.range("t-one").is_none());
        click(&selection, "m-two", 6, 1, false);
        click(&selection, "m-one", 6, 1, true);
        assert_eq!(selection.text(), "world\nlet x = 1;\nSecond");
    }
    #[test]
    fn double_click_and_word_extension_keep_whole_words() {
        let selection = selection();
        click(&selection, "m-one", 2, 2, false);
        assert_eq!(selection.text(), "Hello");
        click(&selection, "m-one", 8, 2, true);
        assert_eq!(selection.text(), "Hello world");
        click(&selection, "m-one", 8, 2, false);
        click(&selection, "m-one", 2, 2, true);
        assert_eq!(selection.text(), "Hello world");
    }
    #[test]
    fn paragraphs_whitespace_and_unicode_have_safe_boundaries() {
        assert_eq!(span("one\ntwo\nthree", 5, Unit::Paragraph), 4..8);
        assert_eq!(span("one   two", 4, Unit::Word), 3..6);
        assert_eq!(span("zażółć gęślą", 5, Unit::Word), 0..10);
        let text = "a👨‍👩‍👧‍👦e\u{301}z";
        assert_eq!(span(text, 5, Unit::Character), 1..1);
        let accent = text.find('e').unwrap();
        assert_eq!(span(text, accent + 2, Unit::Character), accent..accent);
        let selection = Selection::default();
        selection.prepare("unicode".into(), vec![("m-text".into(), text)]);
        click(&selection, "m-text", 1, 1, false);
        click(&selection, "m-text", accent, 1, true);
        assert_eq!(selection.text(), "👨‍👩‍👧‍👦");
    }
    #[test]
    fn shift_click_keeps_anchor_and_new_click_collapses_selection() {
        let selection = selection();
        click(&selection, "m-one", 6, 1, false);
        click(&selection, "m-one", 11, 1, true);
        assert_eq!(selection.text(), "world");
        click(&selection, "m-one", 0, 1, true);
        assert_eq!(selection.text(), "Hello ");
        click(&selection, "m-code", 2, 1, false);
        assert_eq!(selection.text(), "");
    }
    #[test]
    fn select_all_stays_in_original_pane_and_refresh_retains_selection() {
        let selection = selection();
        click(&selection, "m-one", 1, 1, false);
        selection.select_all();
        assert_eq!(selection.text(), "Hello world\nlet x = 1;\nSecond message");
        click(&selection, "t-one", 0, 1, false);
        selection.select_all();
        assert_eq!(selection.text(), "Thread reply");
        selection.prepare(
            "room".into(),
            vec![
                ("m-older".into(), "Older"),
                ("t-one".into(), "Thread reply"),
            ],
        );
        assert_eq!(selection.text(), "Thread reply");
        selection.prepare(
            "another room".into(),
            vec![("t-one".into(), "Thread reply")],
        );
        assert_eq!(selection.text(), "");
    }
    #[test]
    fn edits_cannot_leave_invalid_utf8_offsets_or_a_stuck_drag() {
        let selection = Selection::default();
        selection.prepare("room".into(), vec![("m-one".into(), "abc")]);
        click(&selection, "m-one", 1, 1, false);
        click(&selection, "m-one", 2, 1, true);
        selection.prepare("room".into(), vec![("m-one".into(), "ąb")]);
        assert_eq!(selection.text(), "");
        assert!(!selection.dragging());
        selection.begin("m-one", 0, 1, false, point(px(10.), px(10.)));
        assert!(!selection.autoscrolling());
        assert!(!selection.extend(point(px(11.), px(10.))));
        assert!(selection.finish()); // Small hand movements still allow link clicks.
        selection.begin("m-one", 0, 1, false, point(px(10.), px(10.)));
        selection.extend(point(px(20.), px(10.)));
        assert!(!selection.finish()); // A drag never activates a link.
    }
}
