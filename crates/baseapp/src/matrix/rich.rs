use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, ElementId, FontStyle, FontWeight,
    GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString,
    StrikethroughStyle, StyledText, TextLayout, UnderlineStyle, Window, div, px,
};
use ruma::html::{Html, NodeData, NodeRef};
use ui::{ActiveTheme as _, Text};

const MONO: &str = if cfg!(target_os = "windows") {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Marks {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    code: bool,
    spoiler: bool,
}

#[derive(Clone)]
struct Segment {
    range: Range<usize>,
    marks: Marks,
    link: Option<usize>,
}

#[derive(Clone, Default)]
pub struct Line {
    text: String,
    segments: Vec<Segment>,
    links: Vec<String>,
}

#[derive(Clone)]
pub enum Block {
    Text(Line),
    Heading(Line),
    Quote(Vec<Block>),
    Code(String),
    Item {
        depth: usize,
        bullet: String,
        line: Line,
    },
    Rule,
}

pub type Rich = Vec<Block>;

impl Line {
    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    fn push(&mut self, text: &str, marks: Marks, link: Option<&str>) {
        if text.is_empty() {
            return;
        }
        match link {
            Some(link) => {
                let index = self.links.len();
                self.links.push(link.to_owned());
                self.segment(text, marks, Some(index));
            }
            None if marks.code => self.segment(text, marks, None),
            None => {
                let mut at = 0;
                for found in urls(text) {
                    self.segment(&text[at..found.start], marks, None);
                    let index = self.links.len();
                    self.links.push(text[found.clone()].to_owned());
                    self.segment(&text[found.clone()], marks, Some(index));
                    at = found.end;
                }
                self.segment(&text[at..], marks, None);
            }
        }
    }

    fn segment(&mut self, text: &str, marks: Marks, link: Option<usize>) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.segments.push(Segment {
            range: start..self.text.len(),
            marks,
            link,
        });
    }

    fn trim_end(&mut self) {
        let len = self.text.trim_end().len();
        self.text.truncate(len);
        self.segments.retain_mut(|segment| {
            segment.range.end = segment.range.end.min(len);
            segment.range.start < segment.range.end
        });
    }
}

/// Finds bare web addresses so they can be clicked.
fn urls(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("http") {
        let start = at + offset;
        let rest = &text[start..];
        if !(rest.starts_with("https://") || rest.starts_with("http://"))
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            at = start + 4;
            continue;
        }
        let mut end = start + rest.find(char::is_whitespace).unwrap_or(rest.len());
        while end > start && text[..end].ends_with(['.', ',', ')', '!', '?', ':', ';', '"', '\'']) {
            end -= 1;
        }
        if end > start + "https://".len() {
            found.push(start..end);
        }
        at = end.max(start + 4);
    }
    found
}

pub fn plain(body: &str) -> Rich {
    let mut line = Line::default();
    line.push(body.trim_end(), Marks::default(), None);
    vec![Block::Text(line)]
}

pub fn markdown(body: &str) -> Rich {
    let content = ruma::events::room::message::RoomMessageEventContent::text_markdown(body);
    match &content.msgtype {
        ruma::events::room::message::MessageType::Text(text) => match &text.formatted {
            Some(formatted) => html(&formatted.body),
            None => plain(body),
        },
        _ => plain(body),
    }
}

pub fn html(source: &str) -> Rich {
    let document = Html::parse(source);
    let mut builder = Builder::default();
    for node in document.children() {
        builder.node(&node);
    }
    builder.flush();
    builder.blocks
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    line: Line,
    marks: Marks,
    link: Option<String>,
    lists: Vec<Option<u64>>,
    item: Option<String>,
}

impl Builder {
    fn flush(&mut self) {
        self.line.trim_end();
        let line = std::mem::take(&mut self.line);
        if line.is_blank() {
            return;
        }
        match self.item.take() {
            Some(bullet) => self.blocks.push(Block::Item {
                depth: self.lists.len().saturating_sub(1),
                bullet,
                line,
            }),
            None => self.blocks.push(Block::Text(line)),
        }
    }

    fn text(&mut self, text: &str) {
        let mut collapsed = String::with_capacity(text.len());
        let mut space = self.line.text.is_empty() || self.line.text.ends_with(char::is_whitespace);
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !space {
                    collapsed.push(' ');
                    space = true;
                }
            } else {
                collapsed.push(ch);
                space = false;
            }
        }
        self.line.push(&collapsed, self.marks, self.link.as_deref());
    }

    fn children(&mut self, node: &NodeRef) {
        for child in node.children() {
            self.node(&child);
        }
    }

    fn nested(&mut self, node: &NodeRef) -> Vec<Block> {
        let mut inner = Builder {
            marks: self.marks,
            ..Builder::default()
        };
        inner.children(node);
        inner.flush();
        inner.blocks
    }

    fn node(&mut self, node: &NodeRef) {
        let element = match node.data() {
            NodeData::Text(text) => {
                self.text(&text.borrow());
                return;
            }
            NodeData::Element(element) => element,
            _ => {
                self.children(node);
                return;
            }
        };
        let name: &str = &element.name.local;
        let attribute = |key: &str| {
            element
                .attrs
                .borrow()
                .iter()
                .find(|attribute| &*attribute.name.local == key)
                .map(|attribute| attribute.value.to_string())
        };
        let saved = self.marks;
        match name {
            "mx-reply" | "script" | "style" | "head" => {}
            "b" | "strong" => {
                self.marks.bold = true;
                self.children(node);
            }
            "i" | "em" => {
                self.marks.italic = true;
                self.children(node);
            }
            "s" | "del" | "strike" => {
                self.marks.strike = true;
                self.children(node);
            }
            "u" | "ins" => {
                self.marks.underline = true;
                self.children(node);
            }
            "code" => {
                self.marks.code = true;
                self.children(node);
            }
            "span" if attribute("data-mx-spoiler").is_some() => {
                self.marks.spoiler = true;
                self.children(node);
            }
            "a" => {
                let previous = self.link.take();
                self.link = attribute("href");
                self.children(node);
                self.link = previous;
            }
            "img" => {
                if let Some(alt) = attribute("alt").or_else(|| attribute("title")) {
                    self.text(&alt);
                }
            }
            "br" => self.line.push("\n", self.marks, None),
            "p" | "div" | "tr" | "details" | "summary" | "table" | "thead" | "tbody" => {
                self.flush();
                self.children(node);
                self.flush();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.marks.bold = true;
                self.children(node);
                self.line.trim_end();
                let line = std::mem::take(&mut self.line);
                if !line.is_blank() {
                    self.blocks.push(Block::Heading(line));
                }
            }
            "blockquote" => {
                self.flush();
                let blocks = self.nested(node);
                if !blocks.is_empty() {
                    self.blocks.push(Block::Quote(blocks));
                }
            }
            "pre" => {
                self.flush();
                let code = text_of(node);
                let code = code.trim_end_matches('\n');
                if !code.is_empty() {
                    self.blocks.push(Block::Code(code.to_owned()));
                }
            }
            "ul" | "ol" => {
                self.flush();
                let start = attribute("start")
                    .and_then(|start| start.parse().ok())
                    .unwrap_or(1);
                self.lists.push((name == "ol").then_some(start));
                self.children(node);
                self.flush();
                self.lists.pop();
            }
            "li" => {
                self.flush();
                let bullet = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let bullet = format!("{number}.");
                        *number += 1;
                        bullet
                    }
                    _ => "•".to_owned(),
                };
                self.item = Some(bullet);
                self.children(node);
                self.flush();
            }
            "hr" => {
                self.flush();
                self.blocks.push(Block::Rule);
            }
            _ => self.children(node),
        }
        self.marks = saved;
    }
}

fn text_of(node: &NodeRef) -> String {
    let mut out = String::new();
    fn walk(node: &NodeRef, out: &mut String) {
        match node.data() {
            NodeData::Text(text) => out.push_str(&text.borrow()),
            NodeData::Element(element) if &*element.name.local == "br" => out.push('\n'),
            _ => {
                for child in node.children() {
                    walk(&child, out);
                }
            }
        }
    }
    walk(node, &mut out);
    out
}

pub type OnLink = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// `id` is the block, the range is a byte span of its text, and the string is that text.
pub type OnSelect = Rc<dyn Fn(&str, Range<usize>, &str, &mut Window, &mut App)>;

pub fn render(
    id: &str,
    rich: &Rich,
    faded: bool,
    on_link: &OnLink,
    mark: Option<(&str, &Range<usize>)>,
    on_select: &OnSelect,
    cx: &App,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w_0()
        .children(rich.iter().enumerate().map(|(index, block)| {
            render_block(
                &format!("{id}-{index}"),
                block,
                faded,
                on_link,
                mark,
                on_select,
                cx,
            )
        }))
        .into_any_element()
}

fn render_block(
    id: &str,
    block: &Block,
    faded: bool,
    on_link: &OnLink,
    mark: Option<(&str, &Range<usize>)>,
    on_select: &OnSelect,
    cx: &App,
) -> AnyElement {
    let theme = *cx.theme();
    let chosen = mark
        .filter(|(marked, _)| *marked == id)
        .map(|(_, range)| range);
    match block {
        Block::Text(line) => render_line(id, line, faded, on_link, chosen, on_select, cx),
        Block::Heading(line) => div()
            .text_size(theme.text(Text::Large))
            .child(render_line(id, line, faded, on_link, chosen, on_select, cx))
            .into_any_element(),
        Block::Quote(blocks) => div()
            .flex()
            .flex_col()
            .gap_1()
            .pl_3()
            .border_l_4()
            .border_color(theme.border)
            .text_color(theme.muted_foreground)
            .children(blocks.iter().enumerate().map(|(index, block)| {
                render_block(
                    &format!("{id}-{index}"),
                    block,
                    faded,
                    on_link,
                    mark,
                    on_select,
                    cx,
                )
            }))
            .into_any_element(),
        Block::Code(code) => div()
            .w_full()
            .p_2()
            .rounded(theme.radius)
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .font_family(MONO)
            .text_size(theme.text(Text::Small))
            .child(selectable(
                id,
                code,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                chosen,
                on_link,
                on_select,
                cx,
            ))
            .into_any_element(),
        Block::Item {
            depth,
            bullet,
            line,
        } => div()
            .flex()
            .gap_2()
            .pl(px(8. + 18. * *depth as f32))
            .child(
                div()
                    .flex_none()
                    .min_w(px(14.))
                    .text_color(theme.muted_foreground)
                    .child(bullet.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(render_line(id, line, faded, on_link, chosen, on_select, cx)),
            )
            .into_any_element(),
        Block::Rule => div()
            .w_full()
            .h(px(1.))
            .my_1()
            .bg(theme.border)
            .into_any_element(),
    }
}

fn render_line(
    id: &str,
    line: &Line,
    faded: bool,
    on_link: &OnLink,
    selected: Option<&Range<usize>>,
    on_select: &OnSelect,
    cx: &App,
) -> AnyElement {
    let theme = *cx.theme();
    let link_color = theme.primary;
    let mut highlights = Vec::new();
    let mut code = Vec::new();
    let mut clickable = Vec::new();
    let mut targets = Vec::new();
    for segment in &line.segments {
        let marks = segment.marks;
        let mut style = HighlightStyle::default();
        if marks.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if marks.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if marks.strike {
            style.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            });
        }
        if marks.underline {
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            });
        }
        if marks.code {
            style.background_color = Some(theme.secondary);
            code.push((segment.range.clone(), SharedString::new_static(MONO)));
        }
        if marks.spoiler {
            style.background_color = Some(theme.muted_foreground);
            style.color = Some(theme.muted_foreground);
        }
        if let Some(link) = segment.link {
            style.color = Some(link_color);
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(link_color.opacity(0.5)),
                wavy: false,
            });
            clickable.push(segment.range.clone());
            targets.push(line.links[link].clone());
        }
        if style != HighlightStyle::default() {
            highlights.push((segment.range.clone(), style));
        }
    }
    let links = clickable.into_iter().zip(targets).collect();
    div()
        .min_w_0()
        .text_color(if faded {
            theme.muted_foreground
        } else {
            theme.foreground
        })
        .child(selectable(
            id, &line.text, highlights, code, links, selected, on_link, on_select, cx,
        ))
        .into_any_element()
}

fn selectable(
    id: &str,
    text: &str,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    fonts: Vec<(Range<usize>, SharedString)>,
    links: Vec<(Range<usize>, String)>,
    selected: Option<&Range<usize>>,
    on_link: &OnLink,
    on_select: &OnSelect,
    cx: &App,
) -> AnyElement {
    let theme = *cx.theme();
    let range = selected.and_then(|range| {
        let start = boundary(text, range.start);
        let end = boundary(text, range.end).max(start);
        (!start.eq(&end)).then_some(start..end)
    });
    let highlights = match range {
        Some(range) => tint(&highlights, range, theme.selection.opacity(0.45)),
        None => highlights,
    };
    SelectText::new(
        id,
        StyledText::new(SharedString::from(text.to_owned()))
            .with_highlights(highlights)
            .with_font_family_overrides(fonts),
        text.to_owned(),
        links,
        on_link.clone(),
        on_select.clone(),
    )
    .into_any_element()
}

pub(crate) fn boundary(text: &str, mut offset: usize) -> usize {
    if offset > text.len() {
        return text.len();
    }
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn word_at(text: &str, offset: usize) -> Range<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        return 0..0;
    }
    let offset = boundary(text, offset);
    let position = chars
        .iter()
        .position(|(index, _)| *index >= offset)
        .unwrap_or(chars.len());
    let index = if position < chars.len() && chars[position].0 == offset {
        position
    } else {
        position.saturating_sub(1)
    };
    let word = |glyph: char| glyph.is_alphanumeric() || glyph == '_';
    if !word(chars[index].1) {
        let start = chars[index].0;
        let end = chars
            .get(index + 1)
            .map(|glyph| glyph.0)
            .unwrap_or(text.len());
        return start..end;
    }
    let mut start = index;
    while start > 0 && word(chars[start - 1].1) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && word(chars[end].1) {
        end += 1;
    }
    chars[start].0..chars.get(end).map(|glyph| glyph.0).unwrap_or(text.len())
}

/// Paints `selection` over existing highlights. Ranges stay sorted and non-overlapping.
fn tint(
    highlights: &[(Range<usize>, HighlightStyle)],
    selection: Range<usize>,
    color: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if selection.start >= selection.end {
        return highlights.to_vec();
    }
    let paint = |mut style: HighlightStyle| {
        let hidden = style.color.is_some() && style.color == style.background_color;
        style.background_color = Some(color);
        if hidden {
            style.color = None;
        }
        style
    };
    let plain = HighlightStyle {
        background_color: Some(color),
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut covered = selection.start;
    for (range, style) in highlights {
        if range.end <= selection.start {
            out.push((range.clone(), *style));
            continue;
        }
        if range.start >= selection.end {
            if covered < selection.end {
                out.push((covered..selection.end, plain));
                covered = selection.end;
            }
            out.push((range.clone(), *style));
            continue;
        }
        if range.start > covered && covered < selection.end {
            let gap = range.start.min(selection.end);
            if covered < gap {
                out.push((covered..gap, plain));
            }
        }
        if range.start < selection.start {
            out.push((range.start..selection.start, *style));
        }
        let start = range.start.max(selection.start);
        let end = range.end.min(selection.end);
        if start < end {
            out.push((start..end, paint(*style)));
            covered = end;
        }
        if range.end > selection.end {
            out.push((selection.end..range.end, *style));
        }
    }
    if covered < selection.end {
        out.push((covered..selection.end, plain));
    }
    out
}

struct SelectText {
    id: ElementId,
    block: String,
    text: StyledText,
    source: String,
    links: Vec<(Range<usize>, String)>,
    on_link: OnLink,
    on_select: OnSelect,
}

#[derive(Default)]
struct SelectState {
    anchor: Rc<Cell<Option<usize>>>,
}

impl SelectText {
    fn new(
        id: &str,
        text: StyledText,
        source: String,
        links: Vec<(Range<usize>, String)>,
        on_link: OnLink,
        on_select: OnSelect,
    ) -> Self {
        Self {
            id: id.to_owned().into(),
            block: id.to_owned(),
            text,
            source,
            links,
            on_link,
            on_select,
        }
    }
}

impl Element for SelectText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text
            .prepaint(None, inspector_id, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .paint(None, inspector_id, bounds, state, &mut (), window, cx);
        let layout = self.text.layout().clone();
        let block = self.block.clone();
        let source = self.source.clone();
        let links = self.links.clone();
        let on_link = self.on_link.clone();
        let on_select = self.on_select.clone();
        let hitbox = hitbox.clone();
        window.with_element_state(id.unwrap(), |drag: Option<SelectState>, window| {
            let drag = drag.unwrap_or_default();
            let anchor = drag.anchor.clone();
            if hitbox.is_hovered(window) {
                let index = index_at(&layout, &hitbox, window.mouse_position(), source.len());
                let cursor = if links.iter().any(|(range, _)| range.contains(&index)) {
                    CursorStyle::PointingHand
                } else {
                    CursorStyle::IBeam
                };
                window.set_cursor_style(cursor, &hitbox);
            }

            let anchor_down = anchor.clone();
            let layout_down = layout.clone();
            let hitbox_down = hitbox.clone();
            let block_down = block.clone();
            let source_down = source.clone();
            let on_select_down = on_select.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox_down.is_hovered(window)
                {
                    return;
                }
                cx.stop_propagation();
                let index = index_at(
                    &layout_down,
                    &hitbox_down,
                    event.position,
                    source_down.len(),
                );
                if event.click_count >= 3 {
                    anchor_down.set(None);
                    on_select_down(&block_down, 0..source_down.len(), &source_down, window, cx);
                    return;
                }
                if event.click_count == 2 {
                    anchor_down.set(None);
                    on_select_down(
                        &block_down,
                        word_at(&source_down, index),
                        &source_down,
                        window,
                        cx,
                    );
                    return;
                }
                anchor_down.set(Some(index));
                on_select_down(&block_down, index..index, &source_down, window, cx);
            });

            let anchor_move = anchor.clone();
            let layout_move = layout.clone();
            let hitbox_move = hitbox.clone();
            let block_move = block.clone();
            let source_move = source.clone();
            let on_select_move = on_select.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let Some(start) = anchor_move.get() else {
                    return;
                };
                cx.stop_propagation();
                let index = index_at(
                    &layout_move,
                    &hitbox_move,
                    event.position,
                    source_move.len(),
                );
                let range = start.min(index)..start.max(index);
                on_select_move(&block_move, range, &source_move, window, cx);
            });

            let layout_up = layout;
            let hitbox_up = hitbox;
            let block_up = block;
            let source_up = source;
            let on_select_up = on_select;
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                let Some(start) = anchor.take() else {
                    return;
                };
                let index = index_at(&layout_up, &hitbox_up, event.position, source_up.len());
                if start == index {
                    if let Some((_, target)) =
                        links.iter().find(|(range, _)| range.contains(&index))
                    {
                        on_link(target, window, cx);
                    }
                    return;
                }
                let range = start.min(index)..start.max(index);
                on_select_up(&block_up, range, &source_up, window, cx);
            });

            ((), drag)
        });
    }
}

impl IntoElement for SelectText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn index_at(layout: &TextLayout, hitbox: &Hitbox, position: Point<Pixels>, len: usize) -> usize {
    if position.y < hitbox.bounds.top() {
        return 0;
    }
    if position.y >= hitbox.bounds.bottom() {
        return len;
    }
    match layout.index_for_position(position) {
        Ok(index) | Err(index) => index.min(len),
    }
}

/// A short single-line preview, used for replies and thread summaries.
pub fn preview(rich: &Rich) -> String {
    let mut out = String::new();
    for block in rich {
        let text = match block {
            Block::Text(line) | Block::Heading(line) | Block::Item { line, .. } => {
                line.text.clone()
            }
            Block::Quote(_) => continue,
            Block::Code(code) => code.clone(),
            Block::Rule => continue,
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&text.replace('\n', " "));
        if out.len() > 160 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(rich: &Rich) -> Vec<String> {
        rich.iter()
            .map(|block| match block {
                Block::Text(line) => line.text.clone(),
                Block::Heading(line) => format!("# {}", line.text),
                Block::Item { bullet, line, .. } => format!("{bullet} {}", line.text),
                Block::Code(code) => format!("```{code}```"),
                Block::Quote(inner) => format!("> {}", texts(inner).join(" | ")),
                Block::Rule => "---".into(),
            })
            .collect()
    }

    #[test]
    fn parses_common_formatting() {
        let rich = html(
            "<p>Hello <strong>world</strong></p>\n<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n<pre><code>let x = 1;\n</code></pre>",
        );
        assert_eq!(
            texts(&rich),
            ["Hello world", "• one", "• two", "```let x = 1;```"]
        );
        let Block::Text(line) = &rich[0] else {
            panic!()
        };
        assert!(
            line.segments
                .iter()
                .any(|segment| segment.marks.bold && &line.text[segment.range.clone()] == "world")
        );
    }

    #[test]
    fn drops_reply_fallback_and_keeps_links() {
        let rich = html(
            "<mx-reply><blockquote>old</blockquote></mx-reply>see <a href=\"https://example.org\">this</a>",
        );
        assert_eq!(texts(&rich), ["see this"]);
        let Block::Text(line) = &rich[0] else {
            panic!()
        };
        assert_eq!(line.links, ["https://example.org"]);
    }

    #[test]
    fn finds_bare_links() {
        assert_eq!(urls("go to https://matrix.org."), [6..24]);
        assert!(urls("nohttps://x").is_empty());
        let rich = plain("ordered list:\n1. https://a.b/c");
        let Block::Text(line) = &rich[0] else {
            panic!()
        };
        assert_eq!(line.links, ["https://a.b/c"]);
    }

    #[test]
    fn markdown_becomes_bold() {
        let rich = markdown("Hello from **Huddle**!");
        let Block::Text(line) = &rich[0] else {
            panic!()
        };
        assert_eq!(line.text, "Hello from Huddle!");
        assert!(line.segments.iter().any(|segment| segment.marks.bold));
    }

    #[test]
    fn selection_splits_highlights() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let color = gpui::hsla(0.6, 0.5, 0.5, 0.4);
        let painted = tint(&[(2..6, bold)], 0..4, color);
        let ranges: Vec<_> = painted.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, vec![0..2, 2..4, 4..6]);
        assert!(painted[0].1.background_color.is_some());
        assert_eq!(painted[1].1.font_weight, Some(FontWeight::BOLD));
        assert!(painted[1].1.background_color.is_some());
        assert_eq!(painted[2].1.font_weight, Some(FontWeight::BOLD));
        assert!(painted[2].1.background_color.is_none());
    }

    #[test]
    fn words_follow_the_cursor() {
        assert_eq!(word_at("hello world", 1), 0..5);
        assert_eq!(word_at("hello world", 5), 5..6);
        assert_eq!(word_at("hello world", 7), 6..11);
        assert_eq!(word_at("", 3), 0..0);
    }
}
