use gpui::{Context, EventEmitter, Render, Window, div, prelude::*};

use crate::contracts::{EditorEvent, EditorInit, LaneContext};

pub struct EditorPane {
    pub context: LaneContext,
    pub init: EditorInit,
}

impl EditorPane {
    pub fn new(
        context: LaneContext,
        init: EditorInit,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self { context, init }
    }
}

impl EventEmitter<EditorEvent> for EditorPane {}

impl Render for EditorPane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().p_4().child("Rich-text editing is unavailable in this foundation. Stored documents remain unchanged.")
    }
}
