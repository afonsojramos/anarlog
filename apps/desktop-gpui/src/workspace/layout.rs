use gpui::{Context, MouseButton, Render, SharedString, Window, div, prelude::*, px, svg};

use super::{
    navigation::{Route, SlotId},
    shell::{Navigate, WorkspaceView},
};
use crate::ui::theme::{SYSTEM_FONT, theme};

struct Hint(&'static str);

impl Render for Hint {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.border)
            .text_xs()
            .text_color(colors.foreground)
            .child(self.0)
    }
}

fn icon(label: &'static str) -> impl IntoElement {
    let name = match label {
        "Back" => "ArrowLeft02Icon",
        "Forward" => "ArrowRight02Icon",
        "Sidebar" => "SidebarLeftIcon",
        "Contacts" => "UsersIcon",
        "Folders" => "Folder01Icon",
        "Calendar" => "Calendar03Icon",
        "Automations" => "ZapIcon",
        "Pin" | "Unpin" => "PinIcon",
        _ => "FileTextIcon",
    };
    svg().path(format!("workspace/{name}.svg")).size(px(16.))
}

#[derive(Clone)]
struct TabDrag(SlotId);

impl Render for TabDrag {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .rounded(px(6.))
            .bg(theme(window).accent)
            .child("Move tab")
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(window);
        let route = self
            .navigation
            .current()
            .map(|tab| tab.route.clone())
            .unwrap_or(Route::Empty);
        let catalog = self
            .catalogs
            .iter()
            .find(|(catalog, _)| Some(*catalog) == self.active_catalog)
            .map(|(_, view)| view.clone());
        let sidebar = div()
            .w(px(self.sidebar.width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h_10()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        svg()
                            .path("logo.svg")
                            .size(px(22.))
                            .text_color(colors.foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                div()
                                    .id("open-picker")
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_picker(window, cx)
                                    }))
                                    .child(
                                        svg()
                                            .path("Search01Icon.svg")
                                            .size(px(16.))
                                            .text_color(colors.foreground),
                                    ),
                            )
                            .child(
                                div()
                                    .id("new-note")
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.create(false, cx)))
                                    .child(
                                        svg()
                                            .path("FileAddIcon.svg")
                                            .size(px(16.))
                                            .text_color(colors.foreground),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .children(
                        [
                            Route::Empty,
                            Route::Calendar,
                            Route::Contacts,
                            Route::Folders,
                            Route::Templates,
                            Route::Automations,
                        ]
                        .into_iter()
                        .map(|target| {
                            let label = if target == Route::Empty {
                                "Notes"
                            } else {
                                target.label()
                            };
                            div()
                                .id(SharedString::from(label))
                                .size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .tooltip(move |_, cx| cx.new(|_| Hint(label)).into())
                                .bg(if target.same_resource(&route) {
                                    colors.sidebar_accent
                                } else {
                                    colors.background
                                })
                                .hover(|style| style.bg(colors.accent))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.navigate(Navigate::Open(target.clone(), false), cx)
                                }))
                                .child(icon(label))
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(catalog.clone(), |view, catalog| view.child(catalog))
                    .when(catalog.is_none(), |view| {
                        view.flex()
                            .flex_col()
                            .gap_2()
                            .child(div().px_2().child(self.search.clone()))
                            .child(div().flex_1().min_h_0().child(self.library.clone()))
                            .child(
                                div()
                                    .px_2()
                                    .flex()
                                    .justify_between()
                                    .text_xs()
                                    .when(
                                        self.page.as_ref().is_some_and(|page| page.offset > 0),
                                        |view| {
                                            view.child(
                                                div()
                                                    .id("previous-page")
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
                                        },
                                    )
                                    .when(
                                        self.page.as_ref().is_some_and(|page| page.has_more),
                                        |view| {
                                            view.child(
                                                div()
                                                    .id("next-page")
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
                                        },
                                    ),
                            )
                    }),
            )
            .child(
                div().px_2().pb_2().text_xs().child(
                    div()
                        .id("settings")
                        .cursor_pointer()
                        .child("Settings")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(Navigate::Open(Route::settings("app"), false), cx)
                        })),
                ),
            );
        let surface = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.library
                        .update(cx, |library, cx| library.clear_selection(cx));
                }),
            )
            .overflow_hidden()
            .bg(colors.card)
            .border_l_1()
            .border_t_1()
            .border_color(colors.border)
            .rounded_tl(px(8.))
            .child(
                div()
                    .h(px(40.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .id("toggle-sidebar")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|style| style.bg(colors.accent))
                            .tooltip(|_, cx| cx.new(|_| Hint("Toggle sidebar · Mod+B")).into())
                            .cursor_pointer()
                            .child(icon("Sidebar"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar.toggle();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("history-back")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|style| style.bg(colors.accent))
                            .tooltip(|_, cx| cx.new(|_| Hint("Back · Alt+Left")).into())
                            .cursor_pointer()
                            .child(icon("Back"))
                            .text_color(
                                if self.navigation.current().is_some_and(|tab| tab.can_back()) {
                                    colors.foreground
                                } else {
                                    colors.muted_foreground
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.navigate(Navigate::History(false), cx)
                            })),
                    )
                    .child(
                        div()
                            .id("history-forward")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|style| style.bg(colors.accent))
                            .tooltip(|_, cx| cx.new(|_| Hint("Forward · Alt+Right")).into())
                            .cursor_pointer()
                            .child(icon("Forward"))
                            .text_color(
                                if self
                                    .navigation
                                    .current()
                                    .is_some_and(|tab| tab.can_forward())
                                {
                                    colors.foreground
                                } else {
                                    colors.muted_foreground
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.navigate(Navigate::History(true), cx)
                            })),
                    )
                    .child(
                        div()
                            .id("tabs")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .flex()
                            .h_full()
                            .children(self.navigation.tabs.iter().map(|tab| {
                                let slot = tab.slot;
                                let pinned = tab.pinned;
                                let title = if let Route::Session(id) = &tab.route {
                                    self.titles
                                        .get(id)
                                        .cloned()
                                        .map(SharedString::from)
                                        .unwrap_or("Note".into())
                                } else if let Route::Folder(id) = &tab.route {
                                    self.catalogs
                                        .iter()
                                        .find(|(kind, _)| *kind == super::ports::Catalog::Folders)
                                        .and_then(|(_, view)| view.read(cx).title(id))
                                        .map(SharedString::from)
                                        .unwrap_or_else(|| {
                                            format!(
                                                "Folder · {}",
                                                id.chars().take(8).collect::<String>()
                                            )
                                            .into()
                                        })
                                } else {
                                    tab.route.label().into()
                                };
                                div()
                                    .id(("tab", slot.0))
                                    .on_drag(TabDrag(slot), |drag, _, _, cx| {
                                        cx.new(|_| drag.clone())
                                    })
                                    .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                                        if this.navigation.reorder(drag.0, slot) {
                                            if this.pin_persistence {
                                                cx.emit(
                                                    super::shell::WorkspaceAction::PinnedChanged(
                                                        this.navigation
                                                            .tabs
                                                            .iter()
                                                            .filter(|tab| {
                                                                tab.pinned
                                                                    && tab.route.persistent_pin()
                                                            })
                                                            .map(|tab| tab.route.clone())
                                                            .collect(),
                                                    ),
                                                );
                                            }
                                            cx.notify();
                                        }
                                    }))
                                    .min_w(px(90.))
                                    .max_w(px(200.))
                                    .px_2()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .bg(if self.navigation.active == Some(slot) {
                                        colors.accent
                                    } else {
                                        colors.card
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.navigate(Navigate::Select(slot), cx)
                                    }))
                                    .child(div().flex_1().truncate().text_sm().child(title))
                                    .when(self.pin_persistence && tab.route.pinnable(), |view| {
                                        view.child(
                                            div()
                                                .id(("pin", slot.0))
                                                .size(px(20.))
                                                .rounded_full()
                                                .opacity(if tab.pinned { 1. } else { 0.5 })
                                                .hover(|style| style.bg(colors.sidebar_accent))
                                                .tooltip(move |_, cx| {
                                                    cx.new(|_| {
                                                        Hint(if pinned {
                                                            "Unpin tab"
                                                        } else {
                                                            "Pin tab"
                                                        })
                                                    })
                                                    .into()
                                                })
                                                .child(icon("Pin"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.pin(slot, cx);
                                                })),
                                        )
                                    })
                                    .child(div().id(("close", slot.0)).px_1().child("×").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.navigate(Navigate::Close(slot), cx);
                                        }),
                                    ))
                            })),
                    )
                    .child(
                        div()
                            .id("new-tab")
                            .px_3()
                            .cursor_pointer()
                            .child("+")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.navigate(Navigate::Open(Route::Empty, true), cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(
                        self.route_content
                            .as_ref()
                            .filter(|(target, _)| target == &route)
                            .map(|(_, view)| view.clone()),
                        |view, content| view.child(content),
                    )
                    .when(self.route_content.is_none(), |view| match &route {
                        Route::Session(_) => view.child(self.note.clone()),
                        Route::Calendar => view.child(self.calendar.clone()),
                        Route::Empty => view.child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_4()
                                .child(
                                    svg()
                                        .path("logo.svg")
                                        .size(px(48.))
                                        .text_color(colors.foreground),
                                )
                                .child(div().text_xl().child("What would you like to remember?"))
                                .child(
                                    div()
                                        .id("empty-create")
                                        .px_4()
                                        .py_2()
                                        .rounded(px(8.))
                                        .bg(colors.accent)
                                        .cursor_pointer()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.create(false, cx)),
                                        )
                                        .child("New note"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_foreground)
                                        .child("Open a note with Mod+K"),
                                ),
                        ),
                        _ if catalog.is_some() => {
                            view.child(catalog.as_ref().unwrap().read(cx).detail())
                        }
                        _ => view.child(div().p_6().text_sm().child(format!(
                            "{} content has not been connected to the workspace.",
                            route.label()
                        ))),
                    }),
            )
            .when(!self.message.is_empty(), |view| {
                view.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(self.message.clone()),
                )
            });
        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .pl(px(4.))
            .gap(px(4.))
            .overflow_hidden()
            .font_family(SYSTEM_FONT)
            .text_color(colors.foreground)
            .bg(colors.background)
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::shortcuts))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.resizing {
                    this.sidebar.resize(f32::from(event.position.x) - 4.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.resizing = false),
            )
            .when(self.sidebar.expanded, |view| {
                view.child(sidebar).child(
                    div()
                        .id("sidebar-resizer")
                        .absolute()
                        .left(px(self.sidebar.width + 4.))
                        .w(px(4.))
                        .h_full()
                        .cursor_col_resize()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.resizing = true),
                        ),
                )
            })
            .child(surface)
            .when_some(self.note_operation.clone(),|view,(ids,moving)| view.child(
                div().absolute().inset_0().bg(gpui::rgba(0x00000055)).flex().items_center().justify_center().child(
                    div().w(px(420.)).p_6().rounded(px(12.)).bg(colors.card).border_1().border_color(colors.border).flex().flex_col().gap_4()
                        .child(div().text_lg().child(format!("{} {} notes?",if moving {"Move"} else {"Delete"},ids.len())))
                        .when(moving,|view| view.child(self.move_target.clone()))
                        .when(!moving,|view| view.child(div().text_sm().child("The selected notes and their related records will be removed from the library.")))
                        .child(div().flex().justify_end().gap_3()
                            .child(div().id("cancel-note-operation").cursor_pointer().px_3().py_2().child("Cancel").on_click(cx.listener(|this,_,_,cx| { if !this.mutation_busy { this.note_operation=None; cx.notify(); } })))
                            .child(div().id("confirm-note-operation").cursor_pointer().px_3().py_2().rounded(px(6.)).bg(colors.accent).child(if self.mutation_busy {"Saving…"} else if moving {"Move notes"} else {"Delete notes"}).on_click(cx.listener(|this,_,_,cx| this.submit_note_operation(cx))))
                        )
                )
            ))
            .when(self.picker_open, |view| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .pl(px(if self.sidebar.expanded {
                            self.sidebar.width
                        } else {
                            0.
                        }))
                        .bg(gpui::rgba(0x00000033))
                        .child(self.picker.clone()),
                )
            })
    }
}
