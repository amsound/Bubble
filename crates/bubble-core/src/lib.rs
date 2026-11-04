//! Bubble's application logic, kept apart from the interface (`bubble-slint`).
//! Nothing here draws: the interface shows state from here and sends requests back.

pub mod app;
pub mod autosave;
pub mod backups;
pub mod cloud;
pub mod crash;
pub mod dialogs;
pub mod instance;
pub mod log;
pub mod menu;
pub mod meter;
pub mod midi;
pub mod outputs;
pub mod prefs;
pub mod view;
pub mod writer;

pub use app::{Core, Intent, PendingAction, Prompts, Snapshot};
