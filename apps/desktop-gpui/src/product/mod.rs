use gpui::{Context, EventEmitter, Render, Window, div, prelude::*};

use crate::contracts::{LaneContext, ProductEvent, ProductRoute};

pub struct ProductPane {
    pub context: LaneContext,
    pub route: ProductRoute,
}

impl ProductPane {
    pub fn new(
        context: LaneContext,
        route: ProductRoute,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self { context, route }
    }
}

impl EventEmitter<ProductEvent> for ProductPane {}

impl Render for ProductPane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_4()
            .child("Product settings and account services are unavailable in this foundation.")
    }
}
