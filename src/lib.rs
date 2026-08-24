//! DuiBi - a local-first text compare and merge tool.
//!
//! Everything happens on this machine: no network code, no telemetry, no
//! uploads. The crate is split so the interesting parts can be tested without
//! opening a window:
//!
//! * [`core`]   - diffing, merging, the text buffer, cleanup tools. Pure logic.
//! * [`config`] - persisted preferences.
//! * [`i18n`]   - interface language.
//! * [`ui`]     - the egui front end.

pub mod app;
pub mod config;
pub mod core;
pub mod i18n;
pub mod ui;
