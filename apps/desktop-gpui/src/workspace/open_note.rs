use std::sync::Arc;

use desktop_runtime::{
    CancellationToken, Generation, OpenSession, RenameSession, RuntimeHandle, SessionId,
};
use gpui::{
    AnyView, Context, Entity, EventEmitter, Render, Subscription, Window, div, prelude::*, px,
};

use crate::{
    contracts::{MeetingIntent, ProductRoute, WorkspaceEvent},
    ui::{
        input::{InputEvent, TextInput},
        theme::theme,
    },
};

pub struct NoteView {
    runtime: RuntimeHandle,
    title: Entity<TextInput>,
    current: Option<Arc<OpenSession>>,
    content: Option<AnyView>,
    message: String,
    busy: bool,
    loading: bool,
    generation: Generation,
    cancellation: CancellationToken,
    _input: Subscription,
}

impl EventEmitter<WorkspaceEvent> for NoteView {}

#[derive(Clone)]
pub enum NoteEvent {
    Opened(Arc<OpenSession>),
    Failed,
    Renamed(Arc<OpenSession>),
}

impl EventEmitter<NoteEvent> for NoteView {}

impl NoteView {
    pub fn new(runtime: RuntimeHandle, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| TextInput::new("Note title", cx));
        let input = cx.subscribe(&title, |this, _, event, cx| {
            match event {
                InputEvent::Submitted => this.rename(cx),
                InputEvent::Rejected => {
                    this.message = "Title input is limited to 4096 bytes.".into()
                }
                InputEvent::Changed => {}
            }
            cx.notify();
        });
        Self {
            runtime,
            title,
            current: None,
            content: None,
            message: "Select a note from your local library.".into(),
            busy: false,
            loading: false,
            generation: Generation::default(),
            cancellation: CancellationToken::new(),
            _input: input,
        }
    }

    pub fn has_unsaved_title(&self, cx: &gpui::App) -> bool {
        self.busy
            || self.current.as_ref().is_some_and(|session| {
                self.title.read(cx).buffer.text.as_str() != session.summary.title.as_ref()
            })
    }

    pub fn set_content(
        &mut self,
        session_id: &SessionId,
        content: AnyView,
        cx: &mut Context<Self>,
    ) {
        if self
            .current
            .as_ref()
            .is_some_and(|session| &session.summary.id == session_id)
        {
            self.content = Some(content);
            self.message.clear();
            cx.notify();
        }
    }

    pub fn cancel_open(&mut self) {
        self.cancellation.cancel();
        self.generation.advance();
        self.loading = false;
    }

    fn restore_title(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(session) = &self.current {
            self.title.update(cx, |title, cx| {
                title.set_text(session.summary.title.to_string(), cx)
            });
            self.message.clear();
            cx.notify();
        }
    }

    pub fn open(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.has_unsaved_title(cx) {
            self.message = "Save or restore your title before changing notes.".into();
            cx.notify();
            return;
        }
        self.cancellation.cancel();
        self.cancellation = CancellationToken::new();
        let generation = self.generation.advance();
        let reply = self.runtime.open_session(id, self.cancellation.clone());
        self.loading = true;
        self.message = "Loading note…".into();
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
                this.loading = false;
                match result {
                    Ok(session) => {
                        if this.has_unsaved_title(cx) {
                            this.message =
                                "The title changed while opening a note. Save or restore it first."
                                    .into();
                            cx.emit(NoteEvent::Failed);
                            cx.notify();
                        } else {
                            this.show(session, cx);
                        }
                    }
                    Err(error) => {
                        this.message = error.to_string();
                        cx.emit(NoteEvent::Failed);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn show(&mut self, session: OpenSession, cx: &mut Context<Self>) {
        self.content = None;
        let session = Arc::new(session);
        cx.emit(NoteEvent::Opened(session.clone()));
        if let Some(document) = &session.note {
            cx.emit(WorkspaceEvent::OpenEditor {
                session_id: session.summary.id.clone(),
                document: document.clone(),
            });
        }
        self.title.update(cx, |title, cx| {
            title.set_text(session.summary.title.to_string(), cx)
        });
        self.message = match &session.note {
            Some(note) => format!(
                "The {} document is loaded. Native editor content has not been connected; stored data is unchanged.",
                note.body_format
            ),
            None => "This session has no note document. Existing data is unchanged.".into(),
        };
        self.current = Some(session);
        cx.notify();
    }

    fn rename(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(session) = &self.current else {
            return;
        };
        let title: Arc<str> = self.title.read(cx).buffer.text.as_str().into();
        if title == session.summary.title {
            return;
        }
        let reply = self.runtime.rename_session(RenameSession {
            base: session.summary.clone(),
            title: title.clone(),
        });
        self.busy = true;
        self.message = "Saving title…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match reply {
                Ok(reply) => reply.receive().await,
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(session) => {
                        this.current = Some(Arc::new(session));
                        cx.emit(NoteEvent::Renamed(this.current.as_ref().unwrap().clone()));
                        this.message = if this.title.read(cx).buffer.text.as_str() == title.as_ref()
                        {
                            "Title saved.".into()
                        } else {
                            "Previous title saved; newer title edits are unsaved.".into()
                        };
                    }
                    Err(error) => {
                        this.message = format!("Title not saved: {error}. Your input is preserved.")
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Drop for NoteView {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for NoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .gap_4()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && this.title.read(cx).buffer.marked.is_none()
                    && this.has_unsaved_title(cx)
                {
                    this.restore_title(cx);
                    cx.stop_propagation();
                }
            }))
            .when(self.current.is_some() && !self.loading, |view| {
                view.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1().child(self.title.clone()))
                        .child(
                            div()
                                .id("save-title")
                                .px_3()
                                .py_2()
                                .rounded(px(8.))
                                .bg(colors.accent)
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.rename(cx)))
                                .child(if self.busy { "Saving…" } else { "Save title" }),
                        )
                        .when(self.has_unsaved_title(cx), |view| {
                            view.child(
                                div()
                                    .id("restore-title")
                                    .px_2()
                                    .py_2()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.restore_title(cx)))
                                    .child("Restore"),
                            )
                        }),
                )
            })
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(self.message.clone()),
            )
            .when_some(self.current.clone(), |view, session| {
                view.child(
                    div().flex().gap_3().children(
                        ["Transcript / audio", "Share", "Export"]
                            .into_iter()
                            .enumerate()
                            .map(|(index, label)| {
                                let id = session.summary.id.clone();
                                div()
                                    .id(("note-service", index))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .child(label)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(match index {
                                            0 => WorkspaceEvent::Meeting(MeetingIntent::Open(
                                                id.clone(),
                                            )),
                                            1 => WorkspaceEvent::Product(ProductRoute::Share(
                                                id.clone(),
                                            )),
                                            _ => WorkspaceEvent::Product(ProductRoute::Export(
                                                id.clone(),
                                            )),
                                        });
                                    }))
                            }),
                    ),
                )
            })
            .when_some(self.content.clone(), |view, content| {
                view.child(div().flex_1().min_h_0().child(content))
            })
    }
}
