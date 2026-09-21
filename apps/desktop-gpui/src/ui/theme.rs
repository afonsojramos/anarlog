use gpui::{SharedString, Window, WindowAppearance, font};

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

pub const RADIUS: f32 = 8.0;

const SYSTEM_FONT: &str = if cfg!(target_os = "macos") {
    ".SystemUIFont"
} else if cfg!(target_os = "windows") {
    "Segoe UI"
} else {
    "sans-serif"
};

pub fn system_font(window: &Window) -> SharedString {
    let text_system = window.text_system();
    let id = text_system.resolve_font(&font(SYSTEM_FONT));
    text_system
        .get_font_for_id(id)
        .expect("resolved system font")
        .family
}

pub fn theme(window: &Window) -> Theme {
    match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
        _ => Theme::light(),
    }
}
