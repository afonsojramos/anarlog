use gpui::{Window, WindowAppearance};

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

pub const RADIUS: f32 = 8.0;

pub const SYSTEM_FONT: &str = if cfg!(target_os = "macos") {
    ".SystemUIFont"
} else if cfg!(target_os = "windows") {
    "Segoe UI"
} else {
    "sans-serif"
};

pub fn theme(window: &Window) -> Theme {
    match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
        _ => Theme::light(),
    }
}
