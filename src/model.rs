use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Result, WipsawError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub host_id: String,
    pub tmux_session: String,
    pub cwd: PathBuf,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceContext {
    pub workspace_id: String,
    pub path: PathBuf,
    /// Either `directory` (the path and its descendants) or `file` (only the
    /// exact file). Context rows are the Middle Manager's durable read scope.
    pub kind: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub tmux_window_id: String,
    pub tmux_window_index: i64,
    pub cwd: PathBuf,
    pub account_id: Option<String>,
    pub codex_home_id: Option<String>,
    pub model_profile_id: Option<String>,
    pub codex_thread_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexThread {
    /// Stable Wipsaw-managed ID. This is distinct from Codex's native thread ID.
    pub id: String,
    pub codex_home_id: String,
    pub account_id: String,
    pub account_alias: String,
    pub native_thread_id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub model_profile_id: Option<String>,
    pub model: String,
    pub model_provider: String,
    pub reasoning_effort: Option<String>,
    pub status: String,
    pub rollout_path: Option<PathBuf>,
    pub native_created_at: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManagerKind {
    Lumbergh,
    MiddleManager,
}

impl Display for ManagerKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Lumbergh => "lumbergh",
            Self::MiddleManager => "middle-manager",
        })
    }
}

impl FromStr for ManagerKind {
    type Err = WipsawError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "lumbergh" => Ok(Self::Lumbergh),
            "middle-manager" => Ok(Self::MiddleManager),
            _ => Err(WipsawError::InvalidInput {
                field: "manager kind",
                message: format!("'{value}' must be lumbergh or middle-manager"),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerSession {
    /// Stable Wipsaw-managed ID. The native Codex thread is created on the
    /// first message and resumed for every later turn.
    pub id: String,
    pub kind: ManagerKind,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    pub source_codex_home_id: String,
    pub native_thread_id: Option<String>,
    pub cwd: PathBuf,
    pub model: String,
    pub reasoning_effort: String,
    pub status: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerMessage {
    pub id: i64,
    pub manager_session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfile {
    pub id: String,
    pub name: String,
    pub provider: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub search: Option<bool>,
    pub sandbox: Option<String>,
    pub approval_policy: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct ModelProfileSettings {
    pub provider: Option<String>,
    pub reasoning_effort: Option<String>,
    pub search: Option<bool>,
    pub sandbox: Option<String>,
    pub approval_policy: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountAuthKind {
    ExistingCodexHome,
    ChatgptSession,
    CodexAccessToken,
    OpenaiApiKey,
}

impl AccountAuthKind {
    pub const fn requires_credential_ref(self) -> bool {
        matches!(self, Self::CodexAccessToken | Self::OpenaiApiKey)
    }
}

impl Display for AccountAuthKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ExistingCodexHome => "existing-codex-home",
            Self::ChatgptSession => "chatgpt-session",
            Self::CodexAccessToken => "codex-access-token",
            Self::OpenaiApiKey => "openai-api-key",
        })
    }
}

impl FromStr for AccountAuthKind {
    type Err = WipsawError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "existing-codex-home" => Ok(Self::ExistingCodexHome),
            "chatgpt-session" => Ok(Self::ChatgptSession),
            "codex-access-token" => Ok(Self::CodexAccessToken),
            "openai-api-key" => Ok(Self::OpenaiApiKey),
            _ => Err(WipsawError::InvalidInput {
                field: "auth kind",
                message: format!(
                    "'{value}' must be existing-codex-home, chatgpt-session, codex-access-token, or openai-api-key"
                ),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountOwnerKind {
    Personal,
    Company,
    Service,
}

impl Display for AccountOwnerKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Personal => "personal",
            Self::Company => "company",
            Self::Service => "service",
        })
    }
}

impl FromStr for AccountOwnerKind {
    type Err = WipsawError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "personal" => Ok(Self::Personal),
            "company" => Ok(Self::Company),
            "service" => Ok(Self::Service),
            _ => Err(WipsawError::InvalidInput {
                field: "owner kind",
                message: format!("'{value}' must be personal, company, or service"),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub alias: String,
    pub auth_kind: AccountAuthKind,
    pub owner_kind: AccountOwnerKind,
    #[serde(skip_serializing)]
    pub credential_ref: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexHome {
    pub id: String,
    pub name: String,
    pub host_id: String,
    pub account_id: String,
    pub account_alias: String,
    pub path: PathBuf,
    pub codex_binary: PathBuf,
    pub created_at: String,
    pub updated_at: String,
}

pub fn validate_display_name(field: &'static str, value: &str) -> Result<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(WipsawError::InvalidInput {
            field,
            message: "must not be empty".to_string(),
        });
    }
    if normalized.chars().count() > 96 {
        return Err(WipsawError::InvalidInput {
            field,
            message: "must be 96 characters or fewer".to_string(),
        });
    }
    if normalized.chars().any(char::is_control) {
        return Err(WipsawError::InvalidInput {
            field,
            message: "must not contain control characters".to_string(),
        });
    }
    Ok(normalized)
}

pub fn validate_credential_ref(value: Option<&str>, required: bool) -> Result<Option<String>> {
    let normalized = value.map(str::trim).filter(|value| !value.is_empty());
    if required && normalized.is_none() {
        return Err(WipsawError::InvalidInput {
            field: "credential reference",
            message: "is required for token and API-key accounts".to_string(),
        });
    }
    if let Some(reference) = normalized {
        let supported = ["secret://", "command://", "stdin://", "codex-home://"];
        if !supported.iter().any(|prefix| reference.starts_with(prefix)) {
            return Err(WipsawError::InvalidInput {
                field: "credential reference",
                message: format!(
                    "must start with one of: {} (do not provide a raw secret)",
                    supported.join(", ")
                ),
            });
        }
        return Ok(Some(reference.to_string()));
    }
    Ok(None)
}

pub fn validate_model_name(value: &str) -> Result<String> {
    validate_setting("model", value, true)?.ok_or_else(|| WipsawError::InvalidInput {
        field: "model",
        message: "must not be empty".to_string(),
    })
}

pub fn validate_profile_settings(settings: &ModelProfileSettings) -> Result<ModelProfileSettings> {
    let sandbox = validate_setting("sandbox", settings.sandbox.as_deref().unwrap_or(""), false)?;
    if let Some(value) = sandbox.as_deref()
        && !["read-only", "workspace-write", "danger-full-access"].contains(&value)
    {
        return Err(WipsawError::InvalidInput {
            field: "sandbox",
            message: "must be read-only, workspace-write, or danger-full-access".to_string(),
        });
    }
    let approval_policy = validate_setting(
        "approval policy",
        settings.approval_policy.as_deref().unwrap_or(""),
        false,
    )?;
    if let Some(value) = approval_policy.as_deref()
        && !["untrusted", "on-request", "never"].contains(&value)
    {
        return Err(WipsawError::InvalidInput {
            field: "approval policy",
            message: "must be untrusted, on-request, or never".to_string(),
        });
    }
    let reasoning_effort = validate_setting(
        "reasoning effort",
        settings.reasoning_effort.as_deref().unwrap_or(""),
        false,
    )?;
    if let Some(value) = reasoning_effort.as_deref()
        && !["none", "minimal", "low", "medium", "high", "xhigh"].contains(&value)
    {
        return Err(WipsawError::InvalidInput {
            field: "reasoning effort",
            message: "must be none, minimal, low, medium, high, or xhigh".to_string(),
        });
    }
    Ok(ModelProfileSettings {
        provider: validate_setting(
            "provider",
            settings.provider.as_deref().unwrap_or(""),
            false,
        )?,
        reasoning_effort,
        search: settings.search,
        sandbox,
        approval_policy,
    })
}

fn validate_setting(field: &'static str, value: &str, required: bool) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            Err(WipsawError::InvalidInput {
                field,
                message: "must not be empty".to_string(),
            })
        } else {
            Ok(None)
        };
    }
    if value.chars().any(char::is_control) || value.chars().count() > 128 {
        return Err(WipsawError::InvalidInput {
            field,
            message: "must be 128 characters or fewer and contain no control characters"
                .to_string(),
        });
    }
    Ok(Some(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        ModelProfileSettings, validate_credential_ref, validate_display_name,
        validate_profile_settings,
    };

    #[test]
    fn display_names_are_trimmed_and_collapsed() {
        assert_eq!(
            validate_display_name("name", "  company   work ").unwrap(),
            "company work"
        );
    }

    #[test]
    fn raw_credential_values_are_rejected() {
        let error = validate_credential_ref(Some("sk-example"), true).unwrap_err();
        assert!(error.to_string().contains("do not provide a raw secret"));
    }

    #[test]
    fn invalid_sandbox_is_rejected() {
        let settings = ModelProfileSettings {
            sandbox: Some("rooty-tooty".to_string()),
            ..ModelProfileSettings::default()
        };
        assert!(validate_profile_settings(&settings).is_err());
    }
}
