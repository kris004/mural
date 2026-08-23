#![allow(clippy::missing_errors_doc)]

//! Daemon-independent mural domain logic.
//!
//! This crate owns configuration, wallpaper-library state, shuffle/history
//! planning, and action-to-transition defaults. It deliberately does not depend
//! on Wayland, EGL, daemon event-loop types, or GPU texture handles.

mod actions;
pub mod config;
pub mod wallpaper;
pub mod world_cache;

pub use config::MuralConfig;
