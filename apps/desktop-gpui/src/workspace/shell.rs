use std::sync::Arc;

use desktop_runtime::{
    CancellationToken, Generation, LibraryPage, LibraryQuery, Reply, RuntimeHandle,
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Subscription, Task, Window,
    div, prelude::*, px, svg,
};

use crate::{
    contracts::{LaneContext, WorkspaceEvent},
    ui::{
        input::{InputEvent, TextInput},
        theme::{SYSTEM_FONT, theme},
    },
};

use super::{
    library::{LibraryView, OpenNote},
    open_note::NoteView,
};

pub struct WorkspaceView {
    runtime: RuntimeHandle,
    search: Entity<TextInput>,
    library: Entity<LibraryView>,
    note: Entity<NoteView>,
    query: LibraryQuery,
    page: Option<Arc<LibraryPage>>,
    generation: Generation,
    cancellation: CancellationToken,
    message: String,
    creating: bool,
    ready: bool,
    subscriptions: Vec<Subscription>,
    watcher: Option<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for WorkspaceView {}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl WorkspaceView {
    pub fn new(
        context: LaneContext,
        ready: Reply<()>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new("Search local titles; press Enter", cx));
        let library = cx.new(LibraryView::new);
        let note = cx.new(|cx| NoteView::new(context.runtime.clone(), cx));
        let search_subscription = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Submitted) {
                this.query.search = this.search.read(cx).buffer.text.as_str().into();
                this.query.offset = 0;
                this.reload(cx);
            } else if matches!(event, InputEvent::Rejected) {
                this.message = "Search input is limited to 4096 bytes.".into();
                cx.notify();
            }
        });
        let open_subscription = cx.subscribe(&library, |this, _, event: &OpenNote, cx| {
            this.note
                .update(cx, |note, cx| note.open(event.0.clone(), cx));
        });
        let note_subscription = cx.subscribe(&note, |_, _, event: &WorkspaceEvent, cx| {
            cx.emit(event.clone());
        });
        cx.spawn(async move |this, cx| {
            let result = ready.receive().await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.ready = true;
                    this.reload(cx);
                    this.watch(cx);
                }
                Err(error) => {
                    this.message = format!("Could not open the isolated library: {error}");
                    cx.notify();
                }
            });
        })
        .detach();
        Self {
            runtime: context.runtime,
            search,
            library,
            note,
            query: LibraryQuery::default(),
            page: None,
            generation: Generation::default(),
            cancellation: CancellationToken::new(),
            message: "Opening isolated local library…".into(),
            creating: false,
            ready: false,
            subscriptions: vec![search_subscription, open_subscription, note_subscription],
            watcher: None,
        }
    }

    pub fn set_status(&mut self, message: String, cx: &mut Context<Self>) {
        self.message = message;
        cx.notify();
    }

    pub fn can_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.note.read(cx).has_unsaved_title(cx) {
            self.set_status("Save or restore the note title before closing.".into(), cx);
            false
        } else {
            true
        }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let reply = self.runtime.watch_library();
        self.watcher = Some(cx.spawn(async move |this, cx| {
            let mut watch = match reply {
                Ok(reply) => match reply.receive().await {
                    Ok(watch) => watch,
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            this.set_status(format!("Library watch failed: {error}"), cx)
                        });
                        return;
                    }
                },
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.set_status(error.to_string(), cx));
                    return;
                }
            };
            while watch.snapshots.changed().await.is_ok() {
                let error = watch
                    .terminal_error()
                    .or_else(|| watch.errors.try_recv().ok());
                if this
                    .update(cx, |this, cx| {
                        if let Some(error) = error {
                            this.set_status(format!("Library watch failed: {error}"), cx);
                        } else {
                            this.reload(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = watch.unsubscribe().await;
        }));
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.ready {
            return;
        }
        self.cancellation.cancel();
        self.cancellation = CancellationToken::new();
        let generation = self.generation.advance();
        let reply = self
            .runtime
            .library(self.query.clone(), self.cancellation.clone());
        self.message = "Loading library…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match reply {
                Ok(reply) => reply.receive().await,
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                match result {
                    Ok(page) => {
                        this.message = if page.items.is_empty() {
                            "No notes match. Create a note to begin.".into()
                        } else {
                            format!(
                                "Showing {}–{} in your isolated library",
                                page.offset + 1,
                                page.offset as usize + page.items.len()
                            )
                        };
                        let page = Arc::new(page);
                        this.page = Some(page.clone());
                        this.library
                            .update(cx, |library, cx| library.set_page(page, cx));
                    }
                    Err(error) => this.message = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating || !self.ready {
            return;
        }
        let reply = self.runtime.create_note("Untitled note".into());
        self.creating = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match reply {
                Ok(reply) => reply.receive().await,
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(session) => {
                        this.note
                            .update(cx, |note, cx| note.open(session.summary.id, cx));
                        this.query.offset = 0;
                        this.reload(cx);
                    }
                    Err(error) => this.message = format!("Note not created: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Drop for WorkspaceView {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.subscriptions.clear();
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        let previous = self.page.as_ref().is_some_and(|page| page.offset > 0);
        let next = self.page.as_ref().is_some_and(|page| page.has_more);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.foreground)
            .font_family(SYSTEM_FONT)
            .child(
                div()
                    .h(px(56.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        svg()
                            .path("logo.svg")
                            .w(px(110.))
                            .h(px(28.))
                            .text_color(colors.foreground),
                    )
                    .child(div().text_sm().child("Native local library"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("new-note")
                            .px_3()
                            .py_2()
                            .rounded(px(8.))
                            .bg(colors.accent)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        svg()
                                            .path("FileAddIcon.svg")
                                            .size(px(18.))
                                            .text_color(colors.foreground),
                                    )
                                    .child(if self.creating {
                                        "Creating…"
                                    } else {
                                        "New note"
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(320.))
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(colors.border)
                            .child(div().p_3().child(self.search.clone()))
                            .child(div().flex_1().min_h_0().child(self.library.clone()))
                            .child(
                                div()
                                    .p_3()
                                    .flex()
                                    .gap_4()
                                    .when(previous, |view| {
                                        view.child(
                                            div()
                                                .id("previous")
                                                .cursor_pointer()
                                                .child("Previous")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.query.offset = this
                                                        .query
                                                        .offset
                                                        .saturating_sub(this.query.limit);
                                                    this.reload(cx);
                                                })),
                                        )
                                    })
                                    .when(next, |view| {
                                        view.child(
                                            div()
                                                .id("next")
                                                .cursor_pointer()
                                                .child("Next")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.query.offset = this
                                                        .query
                                                        .offset
                                                        .saturating_add(this.query.limit);
                                                    this.reload(cx);
                                                })),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("refresh")
                                            .cursor_pointer()
                                            .child("Refresh")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.reload(cx)),
                                            ),
                                    ),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(self.note.clone())),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .border_t_1()
                    .border_color(colors.border)
                    .child(self.message.clone()),
            )
    }
}
