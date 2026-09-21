pub mod automations;
pub mod calendar;
mod catalog;
mod layout;
mod library;
pub mod navigation;
mod open_note;
#[cfg(test)]
mod persistence_tests;
mod picker;
pub mod pins;
pub mod ports;
mod shell;
pub mod templates;

pub use shell::{WorkspaceAction, WorkspaceView};
