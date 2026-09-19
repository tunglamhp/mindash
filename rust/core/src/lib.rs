//! Shared model for MinDash.
//!
//! Everything here is used by both the server and the browser build, which is
//! the main reason a Rust workspace is worth it for this project: the config
//! shape, the widget registry and the wire types are defined exactly once, so
//! the two sides cannot drift apart.

pub mod config;
pub mod widgets;

pub use config::{Config, Device, Link, QuickAction, Settings, Task, ThemeColors};
pub use widgets::{Widget, WidgetKind, WidgetType};
