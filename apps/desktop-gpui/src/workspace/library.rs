use std::sync::Arc;

use desktop_runtime::{LibraryPage, SessionId};
use gpui::{Context, EventEmitter, Render, Window, div, prelude::*, px, uniform_list};

use crate::ui::theme::theme;

pub struct LibraryView {
    page: Arc<LibraryPage>,
}

pub struct OpenNote(pub SessionId);
impl EventEmitter<OpenNote> for LibraryView {}

impl LibraryView {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            page: Arc::new(LibraryPage {
                items: Arc::from([]),
                offset: 0,
                has_more: false,
            }),
        }
    }

    pub fn set_page(&mut self, page: Arc<LibraryPage>, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
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
                            .cursor_pointer()
                            .overflow_hidden()
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenNote(id.clone()))))
                            .child(div().text_sm().child(if item.title.is_empty() {
                                "Untitled note".into()
                            } else {
                                gpui::SharedString::from(item.title.clone())
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .child(gpui::SharedString::from(item.created_at.clone())),
                            )
                    })
                    .collect()
            }),
        )
        .size_full()
    }
}
