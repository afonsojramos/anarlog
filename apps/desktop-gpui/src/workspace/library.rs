use std::{collections::HashSet, sync::Arc};

use desktop_runtime::{LibraryPage, SessionId};
use gpui::{
    Context, EventEmitter, FocusHandle, Render, UniformListScrollHandle, Window, div, prelude::*,
    px, uniform_list,
};

use crate::ui::theme::theme;

pub struct LibraryView {
    page: Arc<LibraryPage>,
    selection: Selection,
    active: Option<SessionId>,
    recording: HashSet<SessionId>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
}

pub enum OpenNote {
    Current(SessionId),
    NewTab(SessionId),
    Window(SessionId),
}
impl EventEmitter<OpenNote> for LibraryView {}

#[derive(Default)]
struct Selection {
    ids: HashSet<SessionId>,
    anchor: Option<SessionId>,
}

impl Selection {
    fn click(&mut self, id: SessionId, visible: &[SessionId], additive: bool, range: bool) {
        if range
            && let Some(from) = self
                .anchor
                .as_ref()
                .and_then(|anchor| visible.iter().position(|id| id == anchor))
            && let Some(to) = visible.iter().position(|item| item == &id)
        {
            if !additive {
                self.ids.clear();
            }
            self.ids
                .extend(visible[from.min(to)..=from.max(to)].iter().cloned());
            return;
        }
        if additive {
            if !self.ids.remove(&id) {
                self.ids.insert(id.clone());
            }
        } else {
            self.ids.clear();
            self.ids.insert(id.clone());
        }
        self.anchor = Some(id);
    }
}

impl LibraryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            page: Arc::new(LibraryPage {
                items: Arc::from([]),
                offset: 0,
                has_more: false,
            }),
            selection: Selection::default(),
            active: None,
            recording: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    pub fn set_page(&mut self, page: Arc<LibraryPage>, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    pub fn set_active(&mut self, active: Option<SessionId>, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    pub fn set_recording(&mut self, id: SessionId, active: bool, cx: &mut Context<Self>) {
        let changed = if active {
            self.recording.insert(id)
        } else {
            self.recording.remove(&id)
        };
        if changed {
            cx.notify();
        }
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selection.ids.is_empty() {
            self.selection = Selection::default();
            cx.notify();
        }
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.secondary() && event.keystroke.key == "a" {
                    this.selection.ids =
                        this.page.items.iter().map(|item| item.id.clone()).collect();
                    cx.stop_propagation();
                    cx.notify();
                } else if event.keystroke.key == "escape" && !this.selection.ids.is_empty() {
                    this.clear_selection(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                uniform_list(
                    "library",
                    self.page.items.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let item = &this.page.items[index];
                                let id = item.id.clone();
                                div()
                                    .id(gpui::SharedString::from(item.id.0.clone()))
                                    .h(px(64.))
                                    .p_3()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .hover(|style| style.bg(colors.accent))
                                    .when(
                                        this.selection.ids.contains(&id)
                                            || this.active.as_ref() == Some(&id),
                                        |view| view.bg(colors.sidebar_accent),
                                    )
                                    .cursor_pointer()
                                    .overflow_hidden()
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, window, cx| {
                                            this.focus.focus(window);
                                            let modifiers = event.modifiers();
                                            let visible = this
                                                .page
                                                .items
                                                .iter()
                                                .map(|row| row.id.clone())
                                                .collect::<Vec<_>>();
                                            this.selection.click(
                                                id.clone(),
                                                &visible,
                                                modifiers.secondary(),
                                                modifiers.shift,
                                            );
                                            if event.click_count() == 2 {
                                                cx.emit(OpenNote::Window(id.clone()));
                                            } else if !modifiers.secondary() && !modifiers.shift {
                                                cx.emit(OpenNote::Current(id.clone()));
                                            }
                                            cx.notify();
                                        },
                                    ))
                                    .on_mouse_down(
                                        gpui::MouseButton::Middle,
                                        cx.listener({
                                            let id = item.id.clone();
                                            move |_, _, _, cx| {
                                                cx.emit(OpenNote::NewTab(id.clone()));
                                                cx.stop_propagation();
                                            }
                                        }),
                                    )
                                    .child(div().text_sm().child(if item.title.is_empty() {
                                        "Untitled note".into()
                                    } else {
                                        gpui::SharedString::from(item.title.clone())
                                    }))
                                    .child(
                                        div().text_xs().text_color(colors.muted_foreground).child(
                                            if this.recording.contains(&item.id) {
                                                "Recording".into()
                                            } else {
                                                gpui::SharedString::from(item.created_at.clone())
                                            },
                                        ),
                                    )
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_selection_uses_stable_visible_ids_and_survives_cancelled_actions() {
        let ids = ["a", "b", "c", "d"].map(|id| SessionId(id.into()));
        let mut selection = Selection::default();
        selection.click(ids[1].clone(), &ids, false, false);
        selection.click(ids[3].clone(), &ids, false, true);
        assert_eq!(selection.ids.len(), 3);
        selection.click(ids[2].clone(), &ids, true, false);
        assert_eq!(selection.ids.len(), 2);
        assert!(selection.ids.contains(&ids[1]));
        selection.click(ids[0].clone(), &ids[..1], false, true);
        assert_eq!(selection.ids, HashSet::from([ids[0].clone()]));
    }
}
