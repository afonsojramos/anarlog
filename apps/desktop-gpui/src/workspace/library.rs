use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Datelike, Local, NaiveDate};
use desktop_runtime::{LibraryPage, SessionId};
use gpui::{
    Context, EventEmitter, FocusHandle, ListAlignment, ListState, Render, Window, div, list,
    prelude::*, px,
};

use crate::ui::theme::theme;

pub struct LibraryView {
    page: Arc<LibraryPage>,
    selection: Selection,
    active: Option<SessionId>,
    recording: HashSet<SessionId>,
    scroll: ListState,
    rows: Vec<TimelineRow>,
    focus: FocusHandle,
    menu: Option<gpui::Point<gpui::Pixels>>,
    menu_index: usize,
}

enum TimelineRow {
    Heading(String),
    Note(usize, String),
}

fn bucket(date: NaiveDate, today: NaiveDate) -> String {
    let days = date.signed_duration_since(today).num_days();
    match days {
        0 => "Today".into(),
        -1 => "Yesterday".into(),
        1 => "Tomorrow".into(),
        -6..=-2 => format!("{} days ago", -days),
        2..=6 => format!("in {days} days"),
        -27..=27 => {
            let weeks = (days.unsigned_abs() + 3) / 7;
            match (days < 0, weeks) {
                (true, 1) => "a week ago".into(),
                (true, _) => format!("{weeks} weeks ago"),
                (false, 1) => "next week".into(),
                (false, _) => format!("in {weeks} weeks"),
            }
        }
        _ => {
            let months = ((date.year() - today.year()) * 12 + date.month() as i32
                - today.month() as i32)
                .unsigned_abs()
                .max(1);
            match (days < 0, months) {
                (true, 1) => "a month ago".into(),
                (true, _) => format!("{months} months ago"),
                (false, 1) => "next month".into(),
                (false, _) => format!("in {months} months"),
            }
        }
    }
}

#[derive(Clone)]
pub enum OpenNote {
    Current(SessionId),
    NewTab(SessionId),
    Window(SessionId),
    Delete(Arc<[SessionId]>),
    Move(Arc<[SessionId]>),
    Pin(Arc<[SessionId]>),
}
impl EventEmitter<OpenNote> for LibraryView {}

#[derive(Default)]
struct Selection {
    ids: HashSet<SessionId>,
    anchor: Option<SessionId>,
    cursor: Option<SessionId>,
}

impl Selection {
    fn click(&mut self, id: SessionId, visible: &[SessionId], additive: bool, range: bool) {
        self.cursor = Some(id.clone());
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
            scroll: ListState::new(0, ListAlignment::Top, px(256.)),
            rows: Vec::new(),
            focus: cx.focus_handle(),
            menu: None,
            menu_index: 0,
        }
    }

    pub fn set_page(&mut self, page: Arc<LibraryPage>, cx: &mut Context<Self>) {
        self.selection
            .ids
            .retain(|id| page.items.iter().any(|item| &item.id == id));
        let mut rows = Vec::new();
        let today = Local::now().date_naive();
        let mut previous = String::new();
        for (index, item) in page.items.iter().enumerate() {
            let date = DateTime::parse_from_rfc3339(&item.created_at)
                .ok()
                .map(|date| date.with_timezone(&Local));
            let heading = date
                .map(|date| bucket(date.date_naive(), today))
                .unwrap_or_else(|| "Undated".into());
            if heading != previous {
                rows.push(TimelineRow::Heading(heading.clone()));
                previous = heading;
            }
            let time = date
                .map(|date| {
                    date.format(
                        if date
                            .date_naive()
                            .signed_duration_since(today)
                            .num_days()
                            .abs()
                            < 7
                        {
                            "%H:%M"
                        } else {
                            "%b %-d, %Y"
                        },
                    )
                    .to_string()
                })
                .unwrap_or_default();
            rows.push(TimelineRow::Note(index, time));
        }
        self.scroll.splice(0..self.rows.len(), rows.len());
        self.rows = rows;
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

    fn menu_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let ids: Arc<[SessionId]> = self
            .page
            .items
            .iter()
            .filter(|item| self.selection.ids.contains(&item.id))
            .map(|item| item.id.clone())
            .collect();
        self.menu = None;
        if ids.is_empty() {
            return;
        }
        match index {
            0 => cx.emit(OpenNote::NewTab(ids[0].clone())),
            1 => cx.emit(OpenNote::Window(ids[0].clone())),
            2 => cx.emit(OpenNote::Pin(ids)),
            3 => cx.emit(OpenNote::Move(ids)),
            _ => cx.emit(OpenNote::Delete(ids)),
        }
        cx.notify();
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.menu.is_some() {
                    match event.keystroke.key.as_str() {
                        "escape" => this.menu = None,
                        "down" => this.menu_index = (this.menu_index + 1) % 5,
                        "up" => this.menu_index = (this.menu_index + 4) % 5,
                        "enter" => this.menu_action(this.menu_index, cx),
                        _ => return,
                    }
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "delete" || event.keystroke.key == "backspace" {
                    this.menu_action(4, cx);
                    cx.stop_propagation();
                    return;
                }
                if matches!(event.keystroke.key.as_str(), "up" | "down")
                    && !this.page.items.is_empty()
                {
                    let visible = this
                        .page
                        .items
                        .iter()
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>();
                    let anchor = this
                        .selection
                        .cursor
                        .as_ref()
                        .and_then(|id| visible.iter().position(|item| item == id));
                    let index = match (event.keystroke.key.as_str(), anchor) {
                        ("up", Some(index)) => index.saturating_sub(1),
                        (_, Some(index)) => (index + 1).min(visible.len() - 1),
                        _ => 0,
                    };
                    this.selection.click(
                        visible[index].clone(),
                        &visible,
                        false,
                        event.keystroke.modifiers.shift,
                    );
                    if !event.keystroke.modifiers.shift {
                        cx.emit(OpenNote::Current(visible[index].clone()));
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "enter" {
                    if let Some(id) = this.selection.anchor.clone() {
                        cx.emit(OpenNote::Current(id));
                    }
                    cx.stop_propagation();
                    return;
                }
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
                list(
                    self.scroll.clone(),
                    cx.processor(move |this, row: usize, _, cx| {
                        let Some(row) = this.rows.get(row) else {
                            return div().into_any_element();
                        };
                        let (index, time) = match row {
                            TimelineRow::Heading(title) => {
                                return div()
                                    .h(px(32.))
                                    .px_3()
                                    .pt_2()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(title.clone())
                                    .into_any_element();
                            }
                            TimelineRow::Note(index, time) => (*index, time.clone()),
                        };
                        let item = &this.page.items[index];
                        let id = item.id.clone();
                        div()
                            .id(gpui::SharedString::from(item.id.0.clone()))
                            .h(px(56.))
                            .px_3()
                            .py_2()
                            .mx_1()
                            .rounded(px(6.))
                            .hover(|style| style.bg(colors.accent))
                            .when(
                                this.selection.ids.contains(&id)
                                    || this.active.as_ref() == Some(&id),
                                |view| view.bg(colors.sidebar_accent),
                            )
                            .cursor_pointer()
                            .overflow_hidden()
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener({
                                    let id = item.id.clone();
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        this.focus.focus(window);
                                        if !this.selection.ids.contains(&id) {
                                            this.selection.ids.clear();
                                            this.selection.ids.insert(id.clone());
                                        }
                                        this.menu = Some(event.position);
                                        this.menu_index = 0;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                }),
                            )
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
                            .child(div().text_xs().text_color(colors.muted_foreground).child(
                                if this.recording.contains(&item.id) {
                                    "Recording".into()
                                } else {
                                    gpui::SharedString::from(time)
                                },
                            ))
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
            .when_some(self.menu, |view, position| {
                view.child(gpui::deferred(
                    gpui::anchored().position(position).snap_to_window().child(
                        div()
                            .id("note-context-menu")
                            .w(px(224.))
                            .p_1()
                            .rounded(px(8.))
                            .bg(colors.card)
                            .border_1()
                            .border_color(colors.border)
                            .shadow_lg()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }))
                            .children(
                                [
                                    "Open in new tab",
                                    "Open in new window",
                                    "Pin selected notes",
                                    "Move to folder…",
                                    "Delete selected notes…",
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(index, label)| {
                                    div()
                                        .id(index)
                                        .h(px(30.))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .text_sm()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .when(index == self.menu_index, |view| {
                                            view.bg(colors.accent)
                                        })
                                        .hover(|style| style.bg(colors.accent))
                                        .child(label)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.menu_action(index, cx)
                                        }))
                                }),
                            ),
                    ),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_buckets_match_the_source_relative_day_week_and_month_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        for (days, label) in [
            (0, "Today"),
            (-1, "Yesterday"),
            (1, "Tomorrow"),
            (-6, "6 days ago"),
            (-7, "a week ago"),
            (-11, "2 weeks ago"),
            (-28, "a month ago"),
            (14, "in 2 weeks"),
        ] {
            assert_eq!(bucket(today + chrono::Duration::days(days), today), label);
        }
    }

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
