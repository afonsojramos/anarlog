use std::sync::Arc;

use desktop_runtime::{
    CancellationToken, Generation, OpenSession, RenameSession, RuntimeHandle, SessionId,
};
use gpui::{
    AnyView, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, Render,
    Subscription, Window, div, prelude::*, px, svg,
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
    rename_pending: bool,
    rename_error: bool,
    recording: bool,
    menu: bool,
    menu_index: usize,
    menu_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for NoteView {}

#[derive(Clone)]
pub enum NoteEvent {
    Opened(Arc<OpenSession>),
    Failed,
    Renamed(Arc<OpenSession>),
    RenameFailed,
    Move(SessionId),
}

impl EventEmitter<NoteEvent> for NoteView {}

impl NoteView {
    pub fn new(runtime: RuntimeHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| TextInput::new("Untitled", cx).inline());
        let input = cx.subscribe_in(&title, window, |this, _, event, window, cx| {
            match event {
                InputEvent::Submitted => {
                    this.rename(cx);
                    window.blur();
                }
                InputEvent::Rejected => {
                    this.message = "Title input is limited to 4096 bytes.".into()
                }
                InputEvent::Changed => {}
            }
            cx.notify();
        });
        let blur = cx.on_blur(&title.focus_handle(cx), window, |this, _, cx| {
            this.rename(cx);
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
            rename_pending: false,
            rename_error: false,
            recording: false,
            menu: false,
            menu_index: 0,
            menu_focus: cx.focus_handle(),
            return_focus: None,
            _subscriptions: vec![input, blur],
        }
    }

    pub fn has_unsaved_title(&self, cx: &gpui::App) -> bool {
        self.busy
            || self.current.as_ref().is_some_and(|session| {
                self.title.read(cx).buffer.text.as_str() != session.summary.title.as_ref()
            })
    }

    pub fn set_recording(&mut self, active: bool, cx: &mut Context<Self>) {
        self.recording = active;
        cx.notify();
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
            if !self.rename_error {
                self.message.clear();
            }
            cx.notify();
        }
    }

    pub fn cancel_open(&mut self) {
        self.cancellation.cancel();
        self.generation.advance();
        self.loading = false;
    }

    pub fn remove_sessions(&mut self, ids: &[SessionId], cx: &mut Context<Self>) {
        if self
            .current
            .as_ref()
            .is_some_and(|session| ids.contains(&session.summary.id))
        {
            self.cancel_open();
            self.current = None;
            self.content = None;
            self.message.clear();
            self.title
                .update(cx, |title, cx| title.set_text(String::new(), cx));
            cx.notify();
        }
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
            self.rename_error = false;
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
        self.menu = false;
        self.rename_pending = false;
        self.rename_error = false;
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

    pub(super) fn rename(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.rename_pending = true;
            return;
        }
        if self.loading {
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
        self.rename_error = false;
        self.message.clear();
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
                        this.message.clear();
                        if std::mem::take(&mut this.rename_pending) {
                            this.rename(cx);
                        }
                    }
                    Err(error) => {
                        this.rename_pending = false;
                        this.rename_error = true;
                        this.message =
                            format!("Title not saved: {error}. Your input is preserved.");
                        cx.emit(NoteEvent::RenameFailed);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = false;
        if self.menu_focus.is_focused(window)
            && let Some(focus) = self.return_focus.take()
        {
            focus.focus(window);
        }
        cx.notify();
    }

    fn choose_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        let Some(session) = &self.current else {
            return;
        };
        let id = session.summary.id.clone();
        cx.emit(match index {
            0 => WorkspaceEvent::Meeting(MeetingIntent::Open(id)),
            1 => WorkspaceEvent::Product(ProductRoute::Share(id)),
            2 => WorkspaceEvent::Product(ProductRoute::Export(id)),
            _ => WorkspaceEvent::AttachFile(id),
        });
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
            .relative()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape"
                    && this.title.focus_handle(cx).is_focused(window)
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
                        .h(px(48.))
                        .flex_shrink_0()
                        .px_2()
                        .items_center()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.blur())
                        .child(
                            div()
                                .id("note-folder")
                                .size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .hover(|view| view.bg(colors.accent))
                                .text_color(colors.muted_foreground)
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(session) = &this.current {
                                        cx.emit(NoteEvent::Move(session.summary.id.clone()));
                                    }
                                }))
                                .child(
                                    svg()
                                        .path("workspace/Folder01Icon.svg")
                                        .size(px(16.))
                                        .text_color(colors.muted_foreground),
                                ),
                        )
                        .child(div().text_color(colors.muted_foreground).child("/"))
                        .child(
                            div()
                                .min_w_0()
                                .max_w(px(224.))
                                .flex_1()
                                .child(self.title.clone()),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("record-note")
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(colors.border)
                                .hover(|view| view.bg(colors.accent))
                                .text_sm()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(session) = &this.current {
                                        cx.emit(WorkspaceEvent::Meeting(if this.recording {
                                            MeetingIntent::Stop
                                        } else {
                                            MeetingIntent::Start {
                                                session_id: session.summary.id.clone(),
                                            }
                                        }));
                                    }
                                }))
                                .child(div().size(px(8.)).rounded_full().bg(gpui::rgb(0xff3344)))
                                .child(if self.recording { "Stop" } else { "Record" }),
                        )
                        .child(
                            div()
                                .id("note-more")
                                .size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .hover(|view| view.bg(colors.accent))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.menu {
                                        this.close_menu(window, cx);
                                    } else {
                                        this.return_focus = window.focused(cx);
                                        this.menu = true;
                                        this.menu_index = 0;
                                        this.menu_focus.focus(window);
                                        cx.notify();
                                    }
                                }))
                                .child("⋯"),
                        ),
                )
            })
            .when(!self.message.is_empty(), |view| {
                view.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(colors.muted_foreground)
                        .child(self.message.clone())
                        .when(self.rename_error, |view| {
                            view.child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .id("retry-title")
                                            .cursor_pointer()
                                            .child("Retry")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.rename(cx)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("restore-title")
                                            .cursor_pointer()
                                            .child("Restore")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.restore_title(cx)
                                                }),
                                            ),
                                    ),
                            )
                        }),
                )
            })
            .when_some(self.content.clone(), |view, content| {
                view.child(div().flex_1().min_h_0().child(content))
            })
            .when(self.menu, |view| {
                view.child(gpui::deferred(
                    div()
                        .id("note-actions")
                        .absolute()
                        .top(px(44.))
                        .right(px(8.))
                        .w(px(224.))
                        .occlude()
                        .p_1()
                        .rounded(px(8.))
                        .shadow_lg()
                        .bg(colors.card)
                        .border_1()
                        .border_color(colors.border)
                        .track_focus(&self.menu_focus)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                        )
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => this.close_menu(window, cx),
                                "up" => this.menu_index = (this.menu_index + 3) % 4,
                                "down" => this.menu_index = (this.menu_index + 1) % 4,
                                "enter" => this.choose_menu(this.menu_index, window, cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .children(
                            ["Transcript / audio", "Share", "Export", "Attach file"]
                                .into_iter()
                                .enumerate()
                                .map(|(index, label)| {
                                    div()
                                        .id(("note-action", index))
                                        .px_3()
                                        .py_1()
                                        .text_sm()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .hover(|view| view.bg(colors.accent))
                                        .when(self.menu_index == index, |view| {
                                            view.bg(colors.accent)
                                        })
                                        .child(label)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.choose_menu(index, window, cx)
                                        }))
                                }),
                        ),
                ))
            })
    }
}
