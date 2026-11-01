//! `nah` as a library: the terminal front-end and the built-in web GUI
//! server. The desktop shell (`crates/desktop`) links this crate to run
//! the same GUI inside a native window.

pub mod ctrlc;
pub mod gui;
pub mod repl;
pub mod setup;
pub mod spinner;
pub mod ui;
pub mod wrap;
