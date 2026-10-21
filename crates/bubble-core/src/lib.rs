//! Bubble's application logic, shared by every interface (the egui app today, the
//! Slint app next). Nothing here draws: interfaces show state from here and send
//! requests back.

pub mod app;
pub mod autosave;
pub mod cloud;
pub mod crash;
pub mod dialogs;
pub mod instance;
pub mod log;
pub mod menu;
pub mod midi;
pub mod prefs;
pub mod view;

pub use app::{Core, Intent, PendingAction, Prompts, Snapshot};
