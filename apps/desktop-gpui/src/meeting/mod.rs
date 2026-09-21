use gpui::{Context, EventEmitter, Render, Window, div, prelude::*};

use crate::contracts::{LaneContext, MeetingEvent, MeetingIntent};

pub struct MeetingPane {
    pub context: LaneContext,
    pub intent: MeetingIntent,
}

impl MeetingPane {
    pub fn new(
        context: LaneContext,
        intent: MeetingIntent,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self { context, intent }
    }
}

impl EventEmitter<MeetingEvent> for MeetingPane {}

impl Render for MeetingPane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().p_4().child(
            "Recording, transcription, playback and meeting AI are unavailable in this foundation.",
        )
    }
}
