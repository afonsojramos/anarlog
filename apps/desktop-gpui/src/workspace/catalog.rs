use std::sync::Arc;

use desktop_runtime::{CancellationToken, Generation, RuntimeHandle};
use futures::{
    FutureExt,
    future::{Either, select},
};
use gpui::{
    Context, Entity, Render, SharedString, Subscription, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};

use crate::ui::{
    input::{InputEvent, TextInput},
    theme::theme,
};

use super::ports::{self, Catalog, CatalogPage, CatalogQuery, CatalogRow, Detail, PAGE_SIZE};

pub struct CatalogView {
    runtime: RuntimeHandle,
    query: CatalogQuery,
    search: Entity<TextInput>,
    page: Arc<CatalogPage>,
    selected: Option<(Arc<str>, Arc<str>)>,
    detail: Entity<DetailView>,
    cancel: CancellationToken,
    generation: Generation,
    scroll: UniformListScrollHandle,
    message: String,
    loading: bool,
    viewer: Option<Arc<str>>,
    _subscription: Subscription,
}

impl CatalogView {
    pub fn new(runtime: RuntimeHandle, catalog: Catalog, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Filter; press Enter", cx));
        let detail = cx.new(|_| DetailView::new(runtime.clone(), catalog));
        let subscription = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Submitted) {
                this.query.search = this.search.read(cx).buffer.text.as_str().into();
                this.query.offset = 0;
                this.load(cx);
            }
        });
        Self {
            runtime,
            query: CatalogQuery {
                catalog,
                search: "".into(),
                offset: 0,
            },
            search,
            detail,
            page: Arc::default(),
            selected: None,
            cancel: CancellationToken::new(),
            generation: Generation::default(),
            scroll: UniformListScrollHandle::new(),
            message: String::new(),
            loading: false,
            viewer: None,
            _subscription: subscription,
        }
    }

    pub fn detail(&self) -> Entity<DetailView> {
        self.detail.clone()
    }

    pub fn set_viewer(&mut self, viewer: Option<Arc<str>>) {
        self.viewer = viewer;
    }

    pub fn select_resource(&mut self, kind: &'static str, id: Arc<str>, cx: &mut Context<Self>) {
        let row = self
            .page
            .rows
            .iter()
            .find(|row| row.kind.as_ref() == kind && row.id == id)
            .cloned()
            .unwrap_or(CatalogRow {
                kind: kind.into(),
                id,
                title: "".into(),
                subtitle: "".into(),
                pinned: false,
                self_contact: false,
            });
        self.choose(row, cx);
    }

    pub fn suspend(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.generation.advance();
        self.detail.update(cx, |detail, _| detail.suspend());
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
        let cancel = self.cancel.clone();
        let generation = self.generation.advance();
        let (sql, params) = self.query.sql_for_viewer(self.viewer.as_deref());
        let reply = self.runtime.watch_query(sql, params);
        let runtime = self.runtime.clone();
        self.loading = true;
        self.message = "Loading…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut watch = match reply {
                Ok(reply) => match reply.receive().await {
                    Ok(watch) => watch,
                    Err(error) => {
                        let _ = this
                            .update(cx, |this, cx| this.fail(generation, error.to_string(), cx));
                        return;
                    }
                },
                Err(error) => {
                    let _ =
                        this.update(cx, |this, cx| this.fail(generation, error.to_string(), cx));
                    return;
                }
            };
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                let rows = watch.snapshots.borrow_and_update().rows.clone();
                let error = watch
                    .terminal_error()
                    .or_else(|| watch.errors.try_recv().ok());
                let result = if let Some(error) = error {
                    Err(error)
                } else {
                    match runtime.read(cancel.clone(), move |_| async move {
                        ports::decode_catalog(&rows)
                    }) {
                        Ok(reply) => reply.receive().await,
                        Err(error) => Err(error),
                    }
                };
                if this
                    .update(cx, |this, cx| {
                        if generation != this.generation {
                            return;
                        }
                        match result {
                            Ok(page) => this.apply(page, cx),
                            Err(error) => this.fail(generation, error.to_string(), cx),
                        }
                    })
                    .is_err()
                {
                    break;
                }
                if watch.terminal_error().is_some() {
                    break;
                }
                match select(
                    watch.snapshots.changed().boxed(),
                    cancel.cancelled().boxed(),
                )
                .await
                {
                    Either::Left((Ok(()), _)) => {}
                    _ => break,
                }
            }
            let _ = watch.unsubscribe().await;
        })
        .detach();
    }

    fn fail(&mut self, generation: Generation, error: String, cx: &mut Context<Self>) {
        if self.generation == generation {
            self.loading = false;
            self.message = format!("Could not refresh: {error}. Previous results retained.");
            cx.notify();
        }
    }

    fn apply(&mut self, page: CatalogPage, cx: &mut Context<Self>) {
        let changed = self.page.as_ref() != &page;
        let was_loading = self.loading;
        self.loading = false;
        self.message = if page.rows.is_empty() {
            "No results".into()
        } else {
            String::new()
        };
        if changed || was_loading {
            self.page = Arc::new(page);
            if self.selected.is_none()
                && let Some(row) = self.page.rows.first()
            {
                self.choose(row.clone(), cx);
            } else if let Some(row) = self
                .page
                .rows
                .iter()
                .find(|row| self.selected.as_ref() == Some(&(row.kind.clone(), row.id.clone())))
            {
                self.detail
                    .update(cx, |detail, cx| detail.load(row.clone(), cx));
            }
        }
        if changed || was_loading {
            cx.notify();
        }
    }

    fn choose(&mut self, row: CatalogRow, cx: &mut Context<Self>) {
        self.selected = Some((row.kind.clone(), row.id.clone()));
        self.detail.update(cx, |detail, cx| detail.load(row, cx));
        cx.notify();
    }
}

impl Drop for CatalogView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Render for CatalogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().px_2().child(self.search.clone()))
            .child(
                div().flex_1().min_h_0().child(
                    uniform_list(
                        "catalog-rows",
                        self.page.rows.len(),
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|index| {
                                    let row = this.page.rows[index].clone();
                                    let selected = this.selected.as_ref()
                                        == Some(&(row.kind.clone(), row.id.clone()));
                                    let title = row.title.clone();
                                    let subtitle = row.subtitle.clone();
                                    div()
                                        .id(SharedString::from(format!("{}:{}", row.kind, row.id)))
                                        .h(px(52.))
                                        .px_2()
                                        .py_1()
                                        .mx_1()
                                        .rounded(px(8.))
                                        .bg(if selected {
                                            colors.sidebar_accent
                                        } else {
                                            colors.background
                                        })
                                        .hover(|style| style.bg(colors.accent))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.choose(row.clone(), cx)
                                        }))
                                        .child(
                                            div()
                                                .text_sm()
                                                .truncate()
                                                .child(SharedString::from(title)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(colors.muted_foreground)
                                                .child(SharedString::from(subtitle)),
                                        )
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(self.scroll.clone())
                    .size_full(),
                ),
            )
            .when(!self.message.is_empty(), |view| {
                view.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(self.message.clone()),
                )
            })
            .child(
                div()
                    .h_8()
                    .px_2()
                    .flex()
                    .justify_between()
                    .when(self.query.offset > 0, |view| {
                        view.child(
                            div()
                                .id("catalog-previous")
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.query.offset =
                                        this.query.offset.saturating_sub(PAGE_SIZE as u32);
                                    this.load(cx);
                                }))
                                .child("Previous"),
                        )
                    })
                    .when(self.page.has_more, |view| {
                        view.child(
                            div()
                                .id("catalog-next")
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.query.offset =
                                        this.query.offset.saturating_add(PAGE_SIZE as u32);
                                    this.load(cx);
                                }))
                                .child("Next"),
                        )
                    }),
            )
    }
}

pub struct DetailView {
    runtime: RuntimeHandle,
    catalog: Catalog,
    row: Option<CatalogRow>,
    detail: Option<Arc<Detail>>,
    cancel: CancellationToken,
    generation: Generation,
    message: String,
}

impl DetailView {
    fn new(runtime: RuntimeHandle, catalog: Catalog) -> Self {
        Self {
            runtime,
            catalog,
            row: None,
            detail: None,
            cancel: CancellationToken::new(),
            generation: Generation::default(),
            message: "Select an item.".into(),
        }
    }

    fn load(&mut self, row: CatalogRow, cx: &mut Context<Self>) {
        if self
            .row
            .as_ref()
            .is_some_and(|previous| previous.id == row.id && previous.kind == row.kind)
            && !self.cancel.is_cancelled()
        {
            if self.row.as_ref() != Some(&row) {
                self.row = Some(row);
                cx.notify();
            }
            return;
        }
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
        let generation = self.generation.advance();
        let changed_entity = self
            .row
            .as_ref()
            .is_none_or(|previous| previous.id != row.id || previous.kind != row.kind);
        if changed_entity {
            self.detail = None;
        }
        self.row = Some(row.clone());
        self.message = "Loading…".into();
        let table = match row.kind.as_ref() {
            "human" => "humans",
            "organization" => "organizations",
            "folder" => "folders",
            "template" => "templates",
            "workflow" => "app_settings",
            _ => {
                self.message = "Unsupported resource.".into();
                return;
            }
        };
        let watch_id = if row.kind.as_ref() == "workflow" {
            "automation_workflows"
        } else {
            &row.id
        };
        let reply = self.runtime.watch_query(
            format!("SELECT id,updated_at FROM {table} WHERE id = ?"),
            vec![serde_json::json!(watch_id)],
        );
        let cancel = self.cancel.clone();
        let runtime = self.runtime.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match reply {
                Ok(reply) => reply.receive().await,
                Err(error) => Err(error),
            };
            let mut watch = match result {
                Ok(watch) => watch,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.message = format!("Could not subscribe to detail: {error}");
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                watch.snapshots.borrow_and_update();
                let result = match watch
                    .terminal_error()
                    .or_else(|| watch.errors.try_recv().ok())
                {
                    Some(error) => Err(error),
                    None => match ports::catalog_detail(&runtime, row.clone(), cancel.clone()) {
                        Ok(reply) => reply.receive().await,
                        Err(error) => Err(error),
                    },
                };
                if this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        match result {
                            Ok(detail) => {
                                if this.detail.as_deref() != Some(&detail)
                                    || !this.message.is_empty()
                                {
                                    this.detail = Some(Arc::new(detail));
                                    this.message.clear();
                                    cx.notify();
                                }
                            }
                            Err(error) => {
                                this.message = format!(
                                    "Could not refresh detail: {error}. Previous detail retained."
                                );
                                cx.notify();
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
                if watch.terminal_error().is_some() {
                    break;
                }
                match select(
                    watch.snapshots.changed().boxed(),
                    cancel.cancelled().boxed(),
                )
                .await
                {
                    Either::Left((Ok(()), _)) => {}
                    _ => break,
                }
            }
            let _ = watch.unsubscribe().await;
        })
        .detach();
    }

    fn suspend(&mut self) {
        self.cancel.cancel();
        self.generation.advance();
    }
}

impl Drop for DetailView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .id("catalog-detail")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div().text_xl().child(
                    self.detail
                        .as_ref()
                        .map(|detail| SharedString::from(detail.title.clone()))
                        .unwrap_or_else(|| self.catalog.label().into()),
                ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(self.catalog.limitation()),
            )
            .when(
                self.row.as_ref().is_some_and(|row| row.self_contact),
                |view| view.child(div().text_sm().child("Your profile")),
            )
            .when(!self.message.is_empty(), |view| {
                view.child(self.message.clone())
            })
            .when_some(self.detail.clone(), |view, detail| {
                view.children(detail.warnings.iter().map(|warning| {
                    div()
                        .text_sm()
                        .text_color(colors.destructive)
                        .child(SharedString::from(warning.clone()))
                }))
                .children(detail.fields.iter().take(200).map(|(key, value)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_foreground)
                                .child(SharedString::from(key.clone())),
                        )
                        .child(div().text_sm().child(SharedString::from(value.clone())))
                }))
                .when(detail.fields.len() > 200, |view| {
                    view.child("Only the first 200 fields are shown. Stored data is unchanged.")
                })
            })
    }
}
