pub mod accessibility;
mod clipboard;
pub mod document;
mod input;
pub mod menu;
pub mod model;
mod pane;
pub mod persistence;
mod sequence;
mod surface;

pub use pane::EditorPane;

#[cfg(test)]
mod tests;
