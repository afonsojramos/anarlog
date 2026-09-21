use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, ShapedLine, TextRun,
    UTF16Selection, UnderlineStyle, Window, canvas, div, fill, point, prelude::*, px, size,
};

use super::{
    text::TextBuffer,
    theme::{RADIUS, theme},
};

#[derive(Clone, Debug)]
pub enum InputEvent {
    Changed,
    Submitted,
    Rejected,
}

pub struct TextInput {
    pub buffer: TextBuffer,
    focus: FocusHandle,
    layout: Option<(Bounds<Pixels>, ShapedLine)>,
    placeholder: String,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self {
            buffer: TextBuffer::default(),
            focus: cx.focus_handle(),
            layout: None,
            placeholder: placeholder.into(),
        }
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.buffer = TextBuffer {
            cursor: text.len(),
            anchor: text.len(),
            text,
            marked: None,
        };
        cx.notify();
    }

    fn changed(&mut self, accepted: bool, cx: &mut Context<Self>) {
        cx.emit(if accepted {
            InputEvent::Changed
        } else {
            InputEvent::Rejected
        });
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.marked.is_some() {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "enter" => cx.emit(InputEvent::Submitted),
            "a" if modifiers.secondary() => {
                self.buffer.anchor = 0;
                self.buffer.cursor = self.buffer.text.len();
            }
            "c" | "x" if modifiers.secondary() => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    self.buffer.text[self.buffer.selection()].into(),
                ));
                if event.keystroke.key == "x" {
                    self.replace_text_in_range(None, "", window, cx);
                }
            }
            "v" if modifiers.secondary() => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace_text_in_range(None, &text, window, cx);
                }
            }
            "left" => self.buffer.move_to(
                if !modifiers.shift && !self.buffer.selection().is_empty() {
                    self.buffer.selection().start
                } else {
                    self.buffer.previous()
                },
                modifiers.shift,
            ),
            "right" => self.buffer.move_to(
                if !modifiers.shift && !self.buffer.selection().is_empty() {
                    self.buffer.selection().end
                } else {
                    self.buffer.next()
                },
                modifiers.shift,
            ),
            "home" => self.buffer.move_to(0, modifiers.shift),
            "end" => self.buffer.move_to(self.buffer.text.len(), modifiers.shift),
            "backspace" | "delete" => {
                if self.buffer.selection().is_empty() {
                    self.buffer.anchor = if event.keystroke.key == "backspace" {
                        self.buffer.previous()
                    } else {
                        self.buffer.next()
                    };
                }
                self.replace_text_in_range(None, "", window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.utf8(range.start)..self.buffer.utf8(range.end);
        *actual = Some(self.buffer.utf16(range.start)..self.buffer.utf16(range.end));
        Some(self.buffer.text[range].into())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = self.buffer.selection();
        Some(UTF16Selection {
            range: self.buffer.utf16(range.start)..self.buffer.utf16(range.end),
            reversed: self.buffer.cursor < self.buffer.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .as_ref()
            .map(|range| self.buffer.utf16(range.start)..self.buffer.utf16(range.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.marked = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accepted = self.buffer.replace(range, text, None);
        self.changed(accepted, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let end = text.encode_utf16().count();
        let accepted = self
            .buffer
            .replace(range, text, Some(selected.unwrap_or(end..end)));
        self.changed(accepted, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (bounds, line) = self.layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(self.buffer.utf8(range.start)),
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(self.buffer.utf8(range.end)),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let (bounds, line) = self.layout.as_ref()?;
        Some(
            self.buffer.utf16(
                line.closest_index_for_x(position.x - bounds.left())
                    .min(self.buffer.text.len()),
            ),
        )
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        let input = cx.entity();
        div()
            .id("text-input")
            .w_full()
            .px_2()
            .py_1()
            .overflow_hidden()
            .border_1()
            .border_color(if self.focus.is_focused(window) {
                colors.ring
            } else {
                colors.input
            })
            .rounded(px(RADIUS))
            .bg(colors.card)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window);
                    if let Some((bounds, line)) = &this.layout {
                        this.buffer.move_to(
                            line.closest_index_for_x(event.position.x - bounds.left())
                                .min(this.buffer.text.len()),
                            event.modifiers.shift,
                        );
                    }
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, (), window, cx| {
                        input.update(cx, |this, cx| {
                            let style = window.text_style();
                            let text = if this.buffer.text.is_empty() {
                                this.placeholder.clone()
                            } else {
                                this.buffer.text.clone()
                            };
                            let run = TextRun {
                                len: text.len(),
                                font: style.font(),
                                color: if this.buffer.text.is_empty() {
                                    colors.muted_foreground
                                } else {
                                    colors.foreground
                                },
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let runs = if let Some(marked) = &this.buffer.marked {
                                vec![
                                    TextRun {
                                        len: marked.start,
                                        ..run.clone()
                                    },
                                    TextRun {
                                        len: marked.len(),
                                        underline: Some(UnderlineStyle {
                                            color: Some(colors.foreground),
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
                                .collect()
                            } else {
                                vec![run]
                            };
                            let line =
                                window
                                    .text_system()
                                    .shape_line(text.into(), px(14.), &runs, None);
                            let selected = this.buffer.selection();
                            if this.focus.is_focused(window) {
                                let left = line.x_for_index(selected.start);
                                let right = line.x_for_index(selected.end);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.left() + left, bounds.top()),
                                        size(
                                            if selected.is_empty() {
                                                px(1.)
                                            } else {
                                                right - left
                                            },
                                            bounds.size.height,
                                        ),
                                    ),
                                    if selected.is_empty() {
                                        colors.foreground
                                    } else {
                                        colors.sidebar_accent
                                    },
                                ));
                            }
                            if let Err(error) =
                                line.paint(bounds.origin, bounds.size.height, window, cx)
                            {
                                tracing::error!(%error, "input text paint failed");
                            }
                            window.handle_input(
                                &this.focus,
                                ElementInputHandler::new(bounds, cx.entity()),
                                cx,
                            );
                            this.layout = Some((bounds, line));
                        });
                    },
                )
                .w_full()
                .h(px(24.)),
            )
    }
}
