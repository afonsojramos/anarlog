use std::{path::PathBuf, sync::Arc, time::Duration};

use desktop_runtime::{DocumentSnapshot, Reply, RuntimeHandle, ServiceError, SessionId};
use gpui::{
    AppContext, Context, Entity, Focusable, Render, Subscription, Window, div, prelude::*, px,
};

use crate::{
    contracts::{
        EditorEvent, EditorInit, LaneContext, MeetingEvent, MeetingIntent, ProductEvent,
        ProductRoute, WorkspaceEvent,
    },
    editor::{EditorPane, menu::EditorRequest},
    meeting::{
        MeetingPane, MeetingServices,
        capture::{CaptureService, Phase},
        playback::{self, Playback},
        recovery,
    },
    product::{OpenWorkspaceSection, ProductPane},
    runtime_bridge,
    ui::theme::theme,
    workspace::{WorkspaceAction, WorkspaceView, navigation::Route, pins},
};

struct ProfileStorage(PathBuf);

impl anlg_storage::StorageRuntime for ProfileStorage {
    fn global_base(&self) -> Result<PathBuf, anlg_storage::Error> {
        Ok(self.0.clone())
    }

    fn vault_base(&self) -> Result<PathBuf, anlg_storage::Error> {
        Ok(self.0.join("vault"))
    }
}

pub struct ApplicationView {
    runtime: RuntimeHandle,
    workspace: Entity<WorkspaceView>,
    editor: Option<Entity<EditorPane>>,
    meeting: Option<Entity<MeetingPane>>,
    product: Option<Entity<ProductPane>>,
    services: Option<MeetingServices>,
    meeting_open: bool,
    closing: bool,
    drained: bool,
    status: String,
    subscriptions: Vec<Subscription>,
    editor_subscriptions: Vec<Subscription>,
    meeting_subscription: Option<Subscription>,
    product_subscriptions: Vec<Subscription>,
    pins_revision: Option<pins::Revision>,
    pins_saving: bool,
    pins_pending: Option<Arc<[Route]>>,
}

impl ApplicationView {
    pub fn new(
        runtime: RuntimeHandle,
        ready: Reply<()>,
        profile: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = cx.new(|cx| {
            WorkspaceView::new(
                LaneContext {
                    runtime: runtime.clone(),
                },
                ready,
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&workspace, window, |this, _, event, window, cx| {
                this.workspace_event(event, window, cx)
            }),
            cx.subscribe(&workspace, |this, _, event, cx| match event {
                WorkspaceAction::OpenShared(_) => this.status(
                    "Shared-note access requires the native account service.",
                    cx,
                ),
                WorkspaceAction::NewNoteWindow | WorkspaceAction::OpenNoteWindow(_) => this.status(
                    "Standalone note windows are not available in this native preview.",
                    cx,
                ),
                WorkspaceAction::PinnedChanged(routes) => this.save_pins(routes.clone(), cx),
            }),
        ];
        let pins_runtime = runtime.clone();
        cx.spawn(async move |this, cx| {
            let mut state = pins_runtime.state();
            loop {
                if matches!(
                    *state.borrow_and_update(),
                    desktop_runtime::RuntimeState::Ready
                ) {
                    break;
                }
                if matches!(*state.borrow(), desktop_runtime::RuntimeState::Closed(_)) {
                    return;
                }
                if state.changed().await.is_err() {
                    return;
                }
            }
            let result = match pins::load(&pins_runtime) {
                Ok(reply) => reply.receive().await,
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok((revision, routes)) => {
                    this.pins_revision = Some(revision);
                    this.workspace.update(cx, |workspace, cx| {
                        workspace.restore_pinned(&routes, cx);
                        workspace.set_pin_persistence(true);
                    });
                }
                Err(error) => this.status(&error.to_string(), cx),
            });
        })
        .detach();
        let startup_runtime = runtime.clone();
        let startup = cx.background_executor().spawn(async move {
            let mut state = startup_runtime.state();
            loop {
                match state.borrow_and_update().clone() {
                    desktop_runtime::RuntimeState::Ready => break,
                    desktop_runtime::RuntimeState::Closed(result) => return result.and(Err(ServiceError::Closed)),
                    _ => {}
                }
                state.changed().await.map_err(|_| ServiceError::Closed)?;
            }
            let storage = Arc::new(ProfileStorage(profile));
            let vault = storage.0.join("vault");
            let capture = CaptureService::spawn(
                startup_runtime.clone(),
                Arc::new(anlg_audio_actual::ActualAudio),
                storage,
                Arc::new(|_| Box::pin(async {
                    Err(ServiceError::Unsupported("Recording requires a validated transcription provider. Native credential and model adapters are not connected.".into()))
                })),
            )?;
            let activities = capture.activities.clone();
            let recovery_runtime = startup_runtime.clone();
            let reports = startup_runtime.service(move |_| async move {
                recovery::recover_startup(
                    &recovery_runtime,
                    vault,
                    activities,
                    Arc::new(|_, _| Box::pin(async {
                        Err(ServiceError::Unsupported("Recovery transcription requires a validated provider; recovery audio and markers are retained.".into()))
                    })),
                ).await
            })?.receive().await?;
            let warnings = reports.into_iter().filter_map(|report| report.result.err().map(|error| format!("{}: {error}", report.session.0))).collect::<Vec<_>>();
            Ok((MeetingServices { capture, playback: Playback::spawn()? }, warnings))
        });
        cx.spawn(async move |this, cx| {
            let result = startup.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((services, warnings)) => {
                    cx.set_global(services.clone());
                    this.services = Some(services);
                    if !warnings.is_empty() {
                        this.status(
                            &format!(
                                "Interrupted capture recovery needs attention: {}",
                                warnings.join("; ")
                            ),
                            cx,
                        );
                    }
                }
                Err(error) => this.status(&format!("Meeting startup failed: {error}"), cx),
            });
        })
        .detach();
        Self {
            runtime,
            workspace,
            editor: None,
            meeting: None,
            product: None,
            services: None,
            meeting_open: false,
            closing: false,
            drained: false,
            status: String::new(),
            subscriptions,
            editor_subscriptions: Vec::new(),
            meeting_subscription: None,
            product_subscriptions: Vec::new(),
            pins_revision: None,
            pins_saving: false,
            pins_pending: None,
        }
    }

    fn save_pins(&mut self, routes: Arc<[Route]>, cx: &mut Context<Self>) {
        if self.pins_saving {
            self.pins_pending = Some(routes);
            return;
        }
        let Some(base) = self.pins_revision.clone() else {
            return;
        };
        let reply = match pins::save(&self.runtime, base, routes.clone()) {
            Ok(reply) => reply,
            Err(error) => {
                self.pins_pending = Some(routes);
                self.status(&format!("Pins not saved: {error}"), cx);
                return;
            }
        };
        self.pins_saving = true;
        cx.spawn(async move |this, cx| {
            let result = reply.receive().await;
            let _ = this.update(cx, |this, cx| {
                this.pins_saving = false;
                match result {
                    Ok(revision) => {
                        this.pins_revision = Some(revision);
                        if let Some(routes) = this.pins_pending.take() {
                            this.save_pins(routes, cx);
                        }
                    }
                    Err(error) => {
                        this.pins_pending.get_or_insert(routes);
                        this.status(
                            &format!("Pins not saved; in-memory tabs retained: {error}"),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    fn status(&mut self, message: &str, cx: &mut Context<Self>) {
        self.status = message.into();
        cx.notify();
    }

    fn workspace_event(
        &mut self,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceEvent::OpenEditor {
                session_id,
                document,
            } => self.open_editor(session_id.clone(), document.clone(), window, cx),
            WorkspaceEvent::Meeting(intent) => self.open_meeting(intent.clone(), window, cx),
            WorkspaceEvent::Product(route) => self.open_product(route.clone(), window, cx),
        }
    }

    fn open_editor(
        &mut self,
        session_id: SessionId,
        document: DocumentSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &self.editor
            && editor.read(cx).init.session_id == session_id
        {
            self.workspace.update(cx, |workspace, cx| {
                workspace.set_editor_content(&session_id, editor.clone().into(), cx)
            });
            return;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).is_dirty())
        {
            self.status(
                "Unsaved editor retained. Save before opening another note.",
                cx,
            );
            return;
        }
        let return_focus = self.workspace.read(cx).focus_handle(cx);
        let editor = cx.new(|cx| {
            EditorPane::new(
                LaneContext {
                    runtime: self.runtime.clone(),
                },
                EditorInit {
                    session_id: session_id.clone(),
                    document,
                    return_focus,
                },
                window,
                cx,
            )
        });
        self.editor_subscriptions = vec![
            cx.subscribe(&editor, |this, editor, event, cx| match event {
                EditorEvent::Dirty { session_id, dirty } => this.workspace.update(cx, |workspace, cx| workspace.set_session_dirty(session_id.clone(), *dirty, cx)),
                EditorEvent::Saved(snapshot) => {
                    let dirty = editor.read(cx).is_dirty();
                    this.workspace.update(cx, |workspace, cx| workspace.set_session_dirty(snapshot.session_id.clone(), dirty, cx));
                }
                EditorEvent::SaveFailed { session_id, error } => {
                    this.workspace.update(cx, |workspace, cx| workspace.set_session_dirty(session_id.clone(), true, cx));
                    this.status(&format!("Save failed; draft retained: {error}"), cx);
                }
                EditorEvent::MentionHuman(id) => this.workspace.update(cx, |workspace, cx| workspace.open_route(Route::Human(id.0.clone()), false, cx)),
                EditorEvent::OpenLink(url) => {
                    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                        cx.open_url(url);
                    } else {
                        this.status("This link scheme is not supported by the native preview.", cx);
                    }
                }
                EditorEvent::OpenAttachment(_) => this.status("Native attachment preview is not available; stored attachment references are preserved.", cx),
            }),
            cx.subscribe(&editor, |this, _, event, cx| match event {
                EditorRequest::OpenSession(id) => this.workspace.update(cx, |workspace, cx| workspace.open_session(SessionId(id.clone().into()), cx)),
                EditorRequest::OpenOrganization(id) => this.workspace.update(cx, |workspace, cx| workspace.open_route(Route::Organization(id.clone().into()), false, cx)),
                EditorRequest::MentionSearch { .. } => {}
            }),
        ];
        self.workspace.update(cx, |workspace, cx| {
            workspace.set_editor_content(&session_id, editor.clone().into(), cx)
        });
        self.editor = Some(editor);
        self.meeting_open = false;
        cx.notify();
    }

    fn open_meeting(&mut self, intent: MeetingIntent, window: &mut Window, cx: &mut Context<Self>) {
        if self.services.is_none() {
            self.status("Meeting services are still starting or recovery failed. Retry after resolving the startup error.", cx);
            return;
        }
        let meeting = cx.new(|cx| {
            MeetingPane::new(
                LaneContext {
                    runtime: self.runtime.clone(),
                },
                intent,
                window,
                cx,
            )
        });
        self.meeting_subscription = Some(cx.subscribe(&meeting, |this, _, event, cx| {
            match event {
                MeetingEvent::Recording { session_id, active } => {
                    this.workspace.update(cx, |workspace, cx| {
                        workspace.set_recording(session_id.clone(), *active, cx)
                    })
                }
                MeetingEvent::OpenSession(id) => this
                    .workspace
                    .update(cx, |workspace, cx| workspace.open_session(id.clone(), cx)),
                MeetingEvent::Failed(error) => this.status(&error.to_string(), cx),
            }
        }));
        self.meeting = Some(meeting);
        self.meeting_open = true;
        cx.notify();
    }

    fn open_product(&mut self, route: ProductRoute, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .product
            .as_ref()
            .is_some_and(|product| !product.update(cx, |product, cx| product.can_close(cx)))
        {
            return;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).is_dirty())
        {
            self.status("Save the note before opening this service.", cx);
            return;
        }
        if !self
            .workspace
            .update(cx, |workspace, cx| workspace.can_close(cx))
        {
            return;
        }
        let product = cx.new(|cx| {
            ProductPane::new(
                LaneContext {
                    runtime: self.runtime.clone(),
                },
                route,
                window,
                cx,
            )
        });
        if let Route::Settings(section) = self.workspace.read(cx).current_route() {
            product.update(cx, |product, cx| product.navigate_section(&section, cx));
        }
        self.product_subscriptions = vec![
            cx.subscribe(&product, |this, _, event, cx| match event {
                ProductEvent::NavigateWorkspace => this.leave_product(Route::Empty, cx),
                ProductEvent::OpenSession(id) => this.leave_product(Route::Session(id.clone()), cx),
                ProductEvent::Failed(error) => this.status(&error.to_string(), cx),
            }),
            cx.subscribe(&product, |this, _, event: &OpenWorkspaceSection, cx| {
                let route = match event.0 {
                    "contacts" => Route::Contacts,
                    "folders" => Route::Folders,
                    "templates" => Route::Templates,
                    "automations" => Route::Automations,
                    "calendar" => Route::Calendar,
                    _ => Route::settings(event.0),
                };
                this.leave_product(route, cx);
            }),
        ];
        self.product = Some(product);
        cx.notify();
    }

    fn leave_product(&mut self, route: Route, cx: &mut Context<Self>) {
        if self
            .product
            .as_ref()
            .is_some_and(|product| !product.update(cx, |product, cx| product.can_close(cx)))
        {
            return;
        }
        self.product = None;
        self.product_subscriptions.clear();
        self.workspace
            .update(cx, |workspace, cx| workspace.open_route(route, false, cx));
        cx.notify();
    }

    pub fn request_quit(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        if self.drained {
            cx.quit();
            return;
        }
        if self.pins_saving || self.pins_pending.is_some() {
            if !self.pins_saving
                && let Some(routes) = self.pins_pending.take()
            {
                self.save_pins(routes, cx);
            }
            self.status(
                "Pinned tabs are still saving; retry quit after the save completes.",
                cx,
            );
            return;
        }
        if self
            .product
            .as_ref()
            .is_some_and(|product| !product.update(cx, |product, cx| product.can_close(cx)))
        {
            self.status("Save or restore product edits before quitting.", cx);
            return;
        }
        let flush = self
            .editor
            .as_ref()
            .filter(|editor| editor.read(cx).is_dirty())
            .map(|editor| editor.update(cx, |editor, _| editor.flush()));
        self.closing = true;
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.set_read_only(true, cx));
        }
        self.status("Finishing saves and capture before quitting…", cx);
        let services = self.services.clone();
        cx.spawn(async move |this, cx| {
            let result: desktop_runtime::Result<()> = async {
                if let Some(flush) = flush { flush.await.map_err(|_| ServiceError::Closed)??; }
                if let Some(services) = services {
                    services.playback.send(playback::Command::Pause)?;
                    let update = services.capture.take_update(u64::MAX);
                    if matches!(update.phase, Phase::Loading | Phase::Listening | Phase::Finalizing) {
                        services.capture.stop()?.await.map_err(|_| ServiceError::Closed)??;
                        let deadline = std::time::Instant::now() + Duration::from_secs(30);
                        loop {
                            let update = services.capture.take_update(u64::MAX);
                            match update.phase {
                                Phase::Idle => break,
                                Phase::Failed => return Err(update.error.unwrap_or(ServiceError::Closed)),
                                _ if std::time::Instant::now() >= deadline => return Err(ServiceError::Failed("Capture is still finalizing; retry after it completes.".into())),
                                _ => { gpui::Timer::after(Duration::from_millis(50)).await; }
                            }
                        }
                    }
                }
                let runtime = this.update(cx, |this, cx| {
                    if !this.workspace.update(cx, |workspace, cx| workspace.can_close(cx)) {
                        return Err(ServiceError::Failed("Save or restore the title and finish pending work before quitting.".into()));
                    }
                    Ok(this.runtime.clone())
                }).map_err(|_| ServiceError::Closed)??;
                let drain = cx.update(|cx| runtime_bridge::drain_before_quit(runtime, cx)).map_err(|_| ServiceError::Closed)?;
                drain.await
            }.await;
            let _ = this.update(cx, |this, cx| {
                this.closing = false;
                match result {
                    Ok(()) => { this.drained = true; cx.quit(); }
                    Err(error) => {
                        if let Some(editor) = &this.editor { editor.update(cx, |editor, cx| editor.set_read_only(false, cx)); }
                        this.status(&format!("Quit paused: {error}"), cx);
                    }
                }
            });
        }).detach();
    }
}

impl Render for ApplicationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        let _ = &self.subscriptions;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.foreground)
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.closing {
                    cx.stop_propagation();
                }
                if event.keystroke.key == "q"
                    && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                {
                    this.request_quit(cx);
                    cx.stop_propagation();
                }
            }))
            .when(!self.closing, |view| {
                view.child(
                    div()
                        .flex()
                        .gap_4()
                        .px_3()
                        .py_1()
                        .when(self.product.is_some(), |view| {
                            view.child(
                                div()
                                    .id("back-workspace")
                                    .cursor_pointer()
                                    .child("Back to workspace")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.leave_product(Route::Empty, cx)
                                    })),
                            )
                        })
                        .child(
                            div()
                                .id("native-quit")
                                .cursor_pointer()
                                .child("Quit")
                                .on_click(cx.listener(|this, _, _, cx| this.request_quit(cx))),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when_some(
                        self.product.clone().filter(|_| !self.closing),
                        |view, product| view.child(product),
                    )
                    .when(self.product.is_none() && !self.closing, |view| {
                        view.child(div().flex_1().min_w_0().child(self.workspace.clone()))
                            .when_some(
                                self.meeting.clone().filter(|_| self.meeting_open),
                                |view, meeting| {
                                    view.child(
                                        div()
                                            .w(px(420.))
                                            .h_full()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .id("hide-meeting")
                                                    .p_2()
                                                    .cursor_pointer()
                                                    .child("Hide transcript")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.meeting_open = false;
                                                        cx.notify();
                                                    })),
                                            )
                                            .child(div().flex_1().min_h_0().child(meeting)),
                                    )
                                },
                            )
                    }),
            )
            .when(!self.status.is_empty(), |view| {
                view.child(div().p_2().text_sm().child(self.status.clone()))
            })
    }
}
