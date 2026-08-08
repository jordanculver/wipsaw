use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum WipsawError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("registry error: {0}")]
    Registry(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{entity} '{value}' was not found")]
    NotFound { entity: &'static str, value: String },

    #[error("{entity} '{value}' already exists")]
    AlreadyExists { entity: &'static str, value: String },

    #[error("invalid {field}: {message}")]
    InvalidInput {
        field: &'static str,
        message: String,
    },

    #[error("Codex home '{path}' is already assigned to account '{account}'")]
    CodexHomeConflict { path: PathBuf, account: String },

    #[error("required executable '{program}' is unavailable: {detail}")]
    ExecutableUnavailable { program: String, detail: String },

    #[error("command failed: {program} {args} (exit {code:?}): {stderr}")]
    CommandFailed {
        program: String,
        args: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("tmux returned malformed output: {0}")]
    MalformedTmuxOutput(String),

    #[error("Codex app-server protocol error: {0}")]
    CodexProtocol(String),

    #[error("timed out after {seconds}s waiting for {operation}")]
    Timeout {
        operation: &'static str,
        seconds: u64,
    },
}

pub type Result<T> = std::result::Result<T, WipsawError>;

impl WipsawError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io_error",
            Self::Registry(_) => "registry_error",
            Self::Json(_) => "json_error",
            Self::NotFound { .. } => "not_found",
            Self::AlreadyExists { .. } => "already_exists",
            Self::InvalidInput { .. } => "invalid_input",
            Self::CodexHomeConflict { .. } => "codex_home_conflict",
            Self::ExecutableUnavailable { .. } => "executable_unavailable",
            Self::CommandFailed { .. } => "command_failed",
            Self::MalformedTmuxOutput(_) => "malformed_tmux_output",
            Self::CodexProtocol(_) => "codex_protocol_error",
            Self::Timeout { .. } => "timeout",
        }
    }
}
