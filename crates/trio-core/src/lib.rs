//! Project model, media discovery, layouts and audio sync math.
//! No GPU or UI code lives here so everything is unit-testable.

pub mod discover;
pub mod layout;
pub mod model;
pub mod probe;
pub mod project;
pub mod sync;

pub use model::*;
