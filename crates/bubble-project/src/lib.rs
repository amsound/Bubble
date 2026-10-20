//! Bubble project model (format v1), validation, asset resolution and
//! crash-safe persistence. No audio or UI dependencies.

pub mod fingerprint;
pub mod io;
pub mod model;
pub mod resolve;
pub mod session;
pub mod validate;

pub use io::{LoadError, SaveError, load, save};
pub use model::*;
pub use resolve::{Resolution, Resolver, make_asset_ref, relink};
pub use validate::{Issue, validate};
