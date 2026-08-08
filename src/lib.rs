pub mod app;
pub mod cli;
pub mod codex;
pub mod doctor;
pub mod error;
pub mod id;
pub mod model;
pub mod paths;
pub mod registry;
pub mod tmux;

pub use app::WipsawApp;
pub use error::{Result, WipsawError};
