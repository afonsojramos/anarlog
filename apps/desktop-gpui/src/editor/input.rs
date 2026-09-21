use std::ops::Range;

use gpui::{
    Bounds, Context, EntityInputHandler, KeyDownEvent, Pixels, Point, UTF16Selection, Window,
};

use super::{
    EditorPane, clipboard,
    model::{EditorModel, Selection},
};

impl EditorPane {
    pub(super) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if let Some(menu) = &mut self.mention {
            match key {
                "up" | "down" => {
                    let len = menu.results.candidates.len();
                    if len > 0 {
                        menu.selected =
                            (menu.selected + if key == "down" { 1 } else { len - 1 }) % len;
                    }
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    self.dismiss_mention();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "enter" => {
                    self.commit_mention(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if let Some(menu) = &mut self.slash {
            match key {
                "up" | "down" => {
                    menu.step(key == "down");
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    self.slash = None;
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                "enter" => {
                    self.slash_commit(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if self.model.as_ref().is_some_and(EditorModel::composing) {
            if key == "escape" {
                let model = self.model.as_mut().expect("model");
                let before = model.revision;
                model.cancel_composition();
                self.edited(before, Ok(()), cx);
                cx.stop_propagation();
            }
            return;
        }
        if modifiers.secondary() {
            match key {
                "c" | "x" => {
                    self.copy(key == "x", cx);
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(text) = item.text() {
                            let metadata = if modifiers.shift {
                                None
                            } else {
                                item.metadata().cloned()
                            };
                            self.paste(text, metadata, cx);
                        } else {
                            self.message = "Image paste needs the attachment import service; clipboard content was not discarded.".into();
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        let Some(model) = &mut self.model else {
            return;
        };
        let before = model.revision;
        let result = match key {
            "b" if modifiers.secondary() => model.toggle_mark("bold"),
            "i" if modifiers.secondary() => model.toggle_mark("italic"),
            "u" if modifiers.secondary() => model.toggle_mark("underline"),
            "`" if modifiers.secondary() => model.toggle_mark("code"),
            "z" if modifiers.secondary() && modifiers.shift => model.redo(),
            "z" if modifiers.secondary() => model.undo(),
            "y" if modifiers.secondary() => model.redo(),
            "a" if modifiers.secondary() => {
                model.select(Selection {
                    anchor: 0,
                    head: model.document.units(),
                });
                Ok(())
            }
            "left" | "right" => model.move_grapheme(key == "right", modifiers.shift),
            "up" | "down" => {
                let caret = self.layouts.values().find_map(|layout| {
                    layout.bounds_for(model.selection.head..model.selection.head)
                });
                if let Some(caret) = caret {
                    let target = gpui::point(
                        caret.left(),
                        caret.top()
                            + if key == "up" {
                                -caret.size.height
                            } else {
                                caret.size.height
                            },
                    );
                    if let Some(layout) = self
                        .layouts
                        .values()
                        .find(|layout| layout.layout.bounds().contains(&target))
                    {
                        let position = layout.position(target);
                        model.select(Selection {
                            anchor: if modifiers.shift {
                                model.selection.anchor
                            } else {
                                position
                            },
                            head: position,
                        });
                    }
                }
                Ok(())
            }
            "home" | "end" => model
                .document
                .resolve(model.selection.head)
                .map(|resolved| {
                    let position = resolved.start
                        + if key == "end" {
                            resolved.node.children.units()
                        } else {
                            0
                        };
                    model.select(Selection {
                        anchor: if modifiers.shift {
                            model.selection.anchor
                        } else {
                            position
                        },
                        head: position,
                    });
                }),
            "backspace" | "delete" => model.delete(key == "delete"),
            "enter" if !modifiers.shift => model.split_block(),
            "tab" => model.indent_list(modifiers.shift),
            "escape" => {
                self.init.return_focus.focus(window);
                Ok(())
            }
            _ => return,
        };
        self.edited(before, result, cx);
        self.update_slash();
        self.update_mention(cx);
        cx.stop_propagation();
    }
}

impl EntityInputHandler for EditorPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text = clipboard::text_for_range(&self.model.as_ref()?.document, range.clone()).ok()?;
        *actual = Some(range);
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.model.as_ref()?.selection;
        Some(UTF16Selection {
            range: selection.range(),
            reversed: selection.head < selection.anchor,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.model.as_ref()?.marked_range()
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(model) = &mut self.model {
            model.commit_composition();
            if let Some(journal) = &self.journal {
                journal.publish(model.revision, model.document.clone());
            }
            cx.notify();
        }
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = &mut self.model else {
            return;
        };
        let before = model.revision;
        let range = range
            .or_else(|| model.marked_range())
            .unwrap_or_else(|| model.selection.range());
        let result = model.type_text(range, text);
        if result.is_ok() {
            model.commit_composition();
        }
        self.edited(before, result, cx);
        self.update_slash();
        self.update_mention(cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = &mut self.model else {
            return;
        };
        let before = model.revision;
        let end = text.encode_utf16().count();
        let result = model.compose(range, text, selected.unwrap_or(end..end));
        self.edited(before, result, cx);
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.layouts
            .values()
            .find_map(|layout| layout.bounds_for(range.clone()))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.hit(position)
    }
}
