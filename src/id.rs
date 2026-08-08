use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, WipsawError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Workspace,
    Tab,
    Account,
    CodexHome,
    ModelProfile,
    CodexThread,
}

impl EntityKind {
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Workspace => "ws",
            Self::Tab => "tab",
            Self::Account => "acct",
            Self::CodexHome => "home",
            Self::ModelProfile => "profile",
            Self::CodexThread => "thread",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WipsawId(String);

impl WipsawId {
    pub fn new(kind: EntityKind) -> Self {
        Self(format!("{}_{}", kind.prefix(), Uuid::now_v7().simple()))
    }

    pub fn parse(kind: EntityKind, value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let expected = format!("{}_", kind.prefix());
        let raw = value
            .strip_prefix(&expected)
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "id",
                message: format!("expected an ID beginning with '{expected}'"),
            })?;
        Uuid::parse_str(raw).map_err(|_| WipsawError::InvalidInput {
            field: "id",
            message: format!("'{value}' is not a valid Wipsaw ID"),
        })?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn tmux_safe_suffix(&self) -> &str {
        self.0
            .split_once('_')
            .map(|(_, suffix)| suffix)
            .unwrap_or(&self.0)
    }
}

impl Display for WipsawId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for WipsawId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::{EntityKind, WipsawId};

    #[test]
    fn generated_ids_have_type_prefix_and_round_trip() {
        let id = WipsawId::new(EntityKind::Workspace);
        assert!(id.as_str().starts_with("ws_"));
        assert_eq!(
            WipsawId::parse(EntityKind::Workspace, id.to_string()).unwrap(),
            id
        );
    }

    #[test]
    fn parser_rejects_the_wrong_entity_kind() {
        let id = WipsawId::new(EntityKind::Tab);
        let error = WipsawId::parse(EntityKind::Workspace, id.to_string()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected an ID beginning with 'ws_'")
        );
    }
}
