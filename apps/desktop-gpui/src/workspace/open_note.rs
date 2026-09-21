use std::sync::Arc;

use desktop_runtime::{
    CancellationToken, Generation, OpenSession, RenameSession, RuntimeHandle, SessionId,
};
use gpui::{Context, Entity, EventEmitter, Render, Subscription, Window, div, prelude::*, px};

use crate::{
    contracts::WorkspaceEvent,
    ui::{
        input::{InputEvent, TextInput},
        theme::theme,
    },
};

pub struct NoteView {
    runtime: RuntimeHandle,
    title: Entity<TextInput>,
    current: Option<Arc<OpenSession>>,
    preview: String,
    message: String,
    busy: bool,
    loading: bool,
    generation: Generation,
    cancellation: CancellationToken,
    _input: Subscription,
}

impl EventEmitter<WorkspaceEvent> for NoteView {}

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
            preview: String::new(),
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
                    Ok(session) => this.show(session, cx),
                    Err(error) => {
                        this.message = error.to_string();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn show(&mut self, session: OpenSession, cx: &mut Context<Self>) {
        if let Some(document) = &session.note {
            cx.emit(WorkspaceEvent::OpenEditor {
                session_id: session.summary.id.clone(),
                document: document.clone(),
            });
        }
        self.title.update(cx, |title, cx| {
            title.set_text(session.summary.title.to_string(), cx)
        });
        self.preview = session
            .note
            .as_ref()
            .map(|note| note.body.chars().take(6000).collect::<String>())
            .unwrap_or_else(|| "No note document is stored for this session.".into());
        self.message = match &session.note {
            Some(note) => format!(
                "Read-only stored {} document (first 6000 characters). Rich-text editing is not implemented in this foundation.",
                note.body_format
            ),
            None => "This session has no note document. Existing data is unchanged.".into(),
        };
        self.current = Some(Arc::new(session));
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
                        ),
                )
            })
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(self.message.clone()),
            )
            .child(
                div()
                    .id("document-preview")
                    .flex_1()
                    .overflow_y_scroll()
                    .text_sm()
                    .child(self.preview.clone()),
            )
    }
}
