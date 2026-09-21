pub mod assets;
pub mod input;
pub mod text;
pub mod theme;

use gpui::{FocusHandle, Window};

#[derive(Clone)]
pub struct FocusReturn(pub FocusHandle);

impl FocusReturn {
    pub fn restore(&self, window: &mut Window) {
        self.0.focus(window);
    }
}
