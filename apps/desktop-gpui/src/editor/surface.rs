use std::{ops::Range, sync::Arc};

use gpui::{
    Bounds, Context, FontStyle, FontWeight, IntoElement, Pixels, Point, SharedString,
    StrikethroughStyle, StyledText, TextLayout, TextRun, UnderlineStyle, Window, canvas, div, fill,
    point, prelude::*, px, size,
};
use serde_json::Value;

use super::{
    EditorPane,
    document::{Document, NodeRef, utf8},
    model::Selection,
    sequence::Measured,
};
use crate::ui::theme::theme;

pub fn block_target(
    document: &Document,
    mut index: usize,
) -> Option<(NodeRef, usize, usize, String)> {
    let mut node = document.root.clone();
    let mut start = 0;
    let mut depth = 0;
    let mut marker = String::new();
    while node.projects_children() {
        let (child_index, remaining, child) = node.children.locate_render(index)?;
        marker = match node.kind() {
            "orderedList" => format!(
                "{}.",
                node.attr("start").and_then(Value::as_u64).unwrap_or(1) + child_index as u64
            ),
            "bulletList" => "•".into(),
            "taskItem" => if node.task_done() { "[x]" } else { "[ ]" }.into(),
            _ if child_index == 0 => marker,
            _ => String::new(),
        };
        depth += usize::from(matches!(
            node.kind(),
            "blockquote" | "bulletList" | "orderedList" | "taskList"
        ));
        start += usize::from(node.kind() != "doc") + node.children.prefix(child_index);
        node = child.clone();
        index = remaining;
    }
    Some((node, start, depth, marker))
}

pub fn block_index(document: &Document, position: usize) -> Option<usize> {
    let resolved = document.resolve(position).ok()?;
    let mut node = document.root.clone();
    let mut index = 0;
    for child_index in resolved.path {
        if !node.projects_children() {
            break;
        }
        index += node.children.render_prefix(child_index);
        node = node.children.get(child_index)?.clone();
    }
    Some(index)
}

#[derive(Clone)]
pub struct Span {
    pub bytes: Range<usize>,
    pub position: usize,
    pub units: usize,
    pub atom: bool,
    pub marks: Vec<Value>,
    pub node: NodeRef,
}

#[derive(Clone)]
pub struct Row {
    pub id: u64,
    pub start: usize,
    pub text: SharedString,
    pub spans: Vec<Span>,
    pub kind: String,
    pub level: u8,
    pub depth: usize,
    pub marker: String,
}

impl Row {
    pub fn position(&self, byte: usize) -> usize {
        let byte = byte.min(self.text.len());
        for span in &self.spans {
            if byte <= span.bytes.end {
                return self.start
                    + span.position
                    + if span.atom {
                        usize::from(byte > span.bytes.start + span.bytes.len() / 2) * span.units
                    } else {
                        self.text[span.bytes.start..byte.max(span.bytes.start)]
                            .encode_utf16()
                            .count()
                    };
            }
        }
        self.start
    }

    pub fn byte(&self, position: usize) -> Option<usize> {
        let offset = position.checked_sub(self.start)?;
        if self.spans.is_empty() {
            return (offset == 0).then_some(0);
        }
        for span in &self.spans {
            if (span.position..=span.position + span.units).contains(&offset) {
                return if span.atom {
                    Some(if offset == span.position {
                        span.bytes.start
                    } else {
                        span.bytes.end
                    })
                } else {
                    utf8(&self.text[span.bytes.clone()], offset - span.position)
                        .ok()
                        .map(|n| n + span.bytes.start)
                };
            }
        }
        None
    }
}

pub struct ProjectedBlock {
    pub source: NodeRef,
    pub rows: Vec<Arc<Row>>,
    pub grid: Option<Grid>,
}

pub struct Grid {
    pub columns: u16,
    pub cells: Vec<GridCell>,
}

pub struct GridCell {
    pub row: i16,
    pub column: i16,
    pub colspan: u16,
    pub rowspan: u16,
    pub header: bool,
    pub rows: Range<usize>,
}

impl ProjectedBlock {
    #[cfg(test)]
    pub fn build(source: NodeRef) -> Self {
        Self::with_context(source, 0, String::new())
    }

    pub fn with_context(source: NodeRef, depth: usize, marker: String) -> Self {
        fn visit(
            node: &NodeRef,
            start: usize,
            depth: usize,
            marker: String,
            rows: &mut Vec<Arc<Row>>,
        ) {
            if node.is_textblock() {
                let mut text = String::new();
                let mut spans = Vec::new();
                let mut position = 0;
                node.children.visit(&mut |child| {
                    let begin = text.len();
                    if let Some(content) = &child.text {
                        text.push_str(&content.as_string());
                    } else if child.kind() == "hardBreak" {
                        text.push('\n');
                    } else {
                        let label = child
                            .attr("label")
                            .or_else(|| child.attr("name"))
                            .or_else(|| child.attr("alt"))
                            .and_then(Value::as_str)
                            .unwrap_or(child.kind());
                        text.push_str(&format!("[{label}]"));
                    }
                    spans.push(Span {
                        bytes: begin..text.len(),
                        position,
                        units: child.units(),
                        atom: child.text.is_none(),
                        marks: child.marks().to_vec(),
                        node: child.clone(),
                    });
                    position += child.units();
                });
                rows.push(Arc::new(Row {
                    id: node.id,
                    start: start + 1,
                    text: text.into(),
                    spans,
                    kind: node.kind().into(),
                    level: node.attr("level").and_then(Value::as_u64).unwrap_or(1) as u8,
                    depth,
                    marker,
                }));
            } else if node.is_atom() || !node.known() {
                let label = node
                    .attr("name")
                    .or_else(|| node.attr("alt"))
                    .or_else(|| node.attr("label"))
                    .and_then(Value::as_str)
                    .unwrap_or(node.kind());
                let text: SharedString = if node.kind() == "horizontalRule" {
                    "────────────".into()
                } else if !node.known() {
                    format!("[Unsupported {} — preserved read-only]", node.kind()).into()
                } else {
                    format!("[{label}]").into()
                };
                rows.push(Arc::new(Row {
                    id: node.id,
                    start,
                    spans: vec![Span {
                        bytes: 0..text.len(),
                        position: 0,
                        units: node.units(),
                        atom: true,
                        marks: Vec::new(),
                        node: node.clone(),
                    }],
                    text,
                    kind: node.kind().into(),
                    level: 0,
                    depth,
                    marker,
                }));
            } else {
                for index in 0..node.children.len() {
                    let child = node.children.get(index).expect("child");
                    let marker = match node.kind() {
                        "orderedList" => format!(
                            "{}.",
                            node.attr("start").and_then(Value::as_u64).unwrap_or(1) + index as u64
                        ),
                        "bulletList" => "•".into(),
                        "taskItem" => {
                            if node.task_done() {
                                "[x]".into()
                            } else {
                                "[ ]".into()
                            }
                        }
                        _ if index == 0 => marker.clone(),
                        _ => String::new(),
                    };
                    visit(
                        child,
                        start + 1 + node.children.prefix(index),
                        depth
                            + usize::from(matches!(
                                node.kind(),
                                "blockquote" | "bulletList" | "orderedList" | "taskList"
                            )),
                        marker,
                        rows,
                    );
                }
            }
        }
        let mut rows = Vec::new();
        visit(&source, 0, depth, marker, &mut rows);
        let grid = if source.kind() == "table" {
            let mut cells = Vec::new();
            let mut occupied = std::collections::HashMap::new();
            let mut columns = 0;
            for ri in 0..source.children.len() {
                let row = source.children.get(ri).expect("row");
                let mut column = 0;
                for ci in 0..row.children.len() {
                    while occupied.get(&column).is_some_and(|until| *until > ri) {
                        column += 1;
                    }
                    let cell = row.children.get(ci).expect("cell");
                    let colspan = cell
                        .attr("colspan")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .clamp(1, 1000) as u16;
                    let rowspan = cell
                        .attr("rowspan")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .clamp(1, 1000) as u16;
                    let start = 2 + source.children.prefix(ri) + row.children.prefix(ci);
                    let end = start + cell.units();
                    let first = rows.partition_point(|row| row.start < start);
                    let last = rows.partition_point(|row| row.start < end);
                    cells.push(GridCell {
                        row: ri as i16 + 1,
                        column: column as i16 + 1,
                        colspan,
                        rowspan,
                        header: cell.kind() == "tableHeader",
                        rows: first..last,
                    });
                    for index in column..column + usize::from(colspan) {
                        occupied.insert(index, ri + usize::from(rowspan));
                    }
                    column += usize::from(colspan);
                    columns = columns.max(column);
                }
            }
            Some(Grid {
                columns: columns as u16,
                cells,
            })
        } else {
            None
        };
        Self { source, rows, grid }
    }
}

#[derive(Clone)]
pub struct VisibleLayout {
    pub row: Arc<Row>,
    pub layout: TextLayout,
    pub block_start: usize,
}

impl VisibleLayout {
    pub fn position(&self, point: Point<Pixels>) -> usize {
        let byte = self
            .layout
            .index_for_position(point)
            .unwrap_or_else(|byte| byte);
        self.block_start + self.row.position(byte)
    }

    pub fn bounds_for(&self, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let start = self.row.byte(range.start.checked_sub(self.block_start)?)?;
        let end = self.row.byte(range.end.checked_sub(self.block_start)?)?;
        let first = self.layout.position_for_index(start)?;
        let last = self.layout.position_for_index(end)?;
        Some(Bounds::new(
            first,
            size((last.x - first.x).max(px(1.)), self.layout.line_height()),
        ))
    }
}

pub fn render_row(
    row: Arc<Row>,
    block_start: usize,
    current: bool,
    in_cell: bool,
    window: &mut Window,
    cx: &mut Context<EditorPane>,
) -> gpui::AnyElement {
    let colors = theme(window);
    let mut base = window.text_style().to_run(0);
    base.color = colors.foreground;
    if row.marker == "[x]" {
        base.color.a *= 0.5;
        base.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        });
    }
    let mut runs = Vec::new();
    for span in &row.spans {
        let mut run = TextRun {
            len: span.bytes.len(),
            ..base.clone()
        };
        for mark in &span.marks {
            match mark.get("type").and_then(Value::as_str).unwrap_or("") {
                "bold" => run.font.weight = FontWeight::BOLD,
                "italic" => run.font.style = FontStyle::Italic,
                "code" => run.font.family = crate::ui::theme::monospace_font(cx),
                "link" | "underline" => {
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        color: None,
                        wavy: false,
                    });
                    if mark["type"] == "link" {
                        run.color = gpui::rgb(0x2563eb).into();
                    }
                }
                "strike" => {
                    run.strikethrough = Some(StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    })
                }
                "highlight" => run.background_color = Some(gpui::rgba(0xfacc1540).into()),
                _ => {}
            }
        }
        if row.kind == "codeBlock" {
            run.font.family = crate::ui::theme::monospace_font(cx);
        }
        if row.kind == "heading" {
            run.font.weight = if row.level == 1 {
                FontWeight::BOLD
            } else {
                FontWeight::SEMIBOLD
            };
        }
        runs.push(run);
    }
    let text = if row.text.is_empty() {
        " ".into()
    } else {
        row.text.clone()
    };
    if runs.is_empty() {
        runs.push(TextRun {
            len: text.len(),
            ..base
        });
    }
    let text = StyledText::new(text).with_runs(runs);
    let layout = text.layout().clone();
    let visible = VisibleLayout {
        row: row.clone(),
        layout: layout.clone(),
        block_start,
    };
    let entity = cx.entity();
    let font_size = if row.kind == "heading" {
        match row.level {
            1 => 20.,
            2 => 18.,
            _ => 16.,
        }
    } else if row.kind == "codeBlock" {
        14.
    } else {
        16.
    };
    let line_height = if row.kind == "heading" {
        match row.level {
            1 => 28.,
            2 => 26.,
            _ => 24.,
        }
    } else if row.kind == "codeBlock" {
        20.
    } else {
        24.
    };
    div()
        .relative()
        .w_full()
        .min_h(px(line_height))
        .py(px(font_size * 0.125))
        .pl(px(if in_cell {
            0.
        } else {
            12. + row.depth as f32 * 20.
        }))
        .pr(px(if in_cell { 0. } else { 12. }))
        .text_size(px(font_size))
        .line_height(px(line_height))
        .when(row.kind == "codeBlock", |div| {
            div.bg(colors.muted).my_2().py_4().rounded_md()
        })
        .when(!row.marker.is_empty(), |div| {
            let marker = row.marker.clone();
            if matches!(marker.as_str(), "[ ]" | "[x]") {
                div.child(
                    gpui::div()
                        .id(("task", row.id))
                        .absolute()
                        .left_0()
                        .cursor_pointer()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if let Some(model) = &mut this.model {
                                    let before = model.revision;
                                    let result = model.toggle_task(block_start + row.start);
                                    this.edited(before, result, cx);
                                }
                                cx.stop_propagation();
                            }),
                        )
                        .child(if marker == "[x]" { "☑" } else { "☐" }),
                )
            } else {
                div.child(div_marker(marker))
            }
        })
        .child(text)
        .child(
            canvas(
                move |_, _, _| (),
                move |_, (), window, app| {
                    entity.update(app, |this, _| {
                        if !current {
                            return;
                        }
                        if let Some(model) = &this.model {
                            paint_selection(
                                &visible,
                                model.selection,
                                this.focus.is_focused(window),
                                window,
                                colors.foreground,
                                colors.sidebar_accent,
                            );
                        }
                        this.layouts.insert(visible.row.id, visible);
                    });
                },
            )
            .absolute()
            .size_full()
            .top_0()
            .left_0(),
        )
        .into_any_element()
}

fn div_marker(marker: String) -> impl IntoElement {
    div().absolute().left_0().child(marker)
}

fn paint_selection(
    layout: &VisibleLayout,
    selection: Selection,
    focused: bool,
    window: &mut Window,
    caret_color: gpui::Hsla,
    selection_color: gpui::Hsla,
) {
    if !focused {
        return;
    }
    let row_start = layout.block_start + layout.row.start;
    let row_end = layout.block_start + layout.row.position(layout.row.text.len());
    let selection = selection.range();
    if selection.end < row_start || selection.start > row_end {
        return;
    }
    let start = selection.start.max(row_start);
    let end = selection.end.min(row_end);
    let Some(a) = layout
        .row
        .byte(start - layout.block_start)
        .and_then(|byte| layout.layout.position_for_index(byte))
    else {
        return;
    };
    let Some(b) = layout
        .row
        .byte(end - layout.block_start)
        .and_then(|byte| layout.layout.position_for_index(byte))
    else {
        return;
    };
    let height = layout.layout.line_height();
    let bounds = layout.layout.bounds();
    if selection.is_empty() {
        window.paint_quad(fill(Bounds::new(a, size(px(1.), height)), caret_color));
    } else {
        let mut y = a.y;
        while y <= b.y {
            let left = if y == a.y { a.x } else { bounds.left() };
            let right = if y == b.y { b.x } else { bounds.right() };
            window.paint_quad(fill(
                Bounds::new(point(left, y), size((right - left).max(px(1.)), height)),
                selection_color.opacity(0.4),
            ));
            y += height;
        }
    }
}
