//! MCP elicitation helpers for user confirmation on destructive or
//! confirmation-worthy tool calls.
//!
//! The flow: the server asks the client to prompt the user via
//! `elicitation/create`. The client returns a [`UserConfirmation`]; the
//! server branches on the outcome. Clients that don't support elicitation
//! fall through as [`ConfirmOutcome::NotSupported`] so callers can preserve
//! backwards-compatible behavior (e.g. require an explicit `force` flag).

use rmcp::{
    service::{ElicitationError, ServiceError},
    Peer, RoleServer,
};
use serde::{Deserialize, Serialize};

/// Response shape the client fills when the server asks for confirmation.
///
/// Kept deliberately simple: a boolean and an optional free-text reason so
/// the server can log *why* the user declined without needing a separate
/// round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UserConfirmation {
    #[schemars(description = "True if the user confirms the action, false otherwise")]
    pub confirmed: bool,
    #[schemars(description = "Optional reason or note from the user")]
    pub reason: Option<String>,
}

rmcp::elicit_safe!(UserConfirmation);

/// Outcome of a [`confirm`] call.
#[derive(Debug)]
pub enum ConfirmOutcome {
    /// User explicitly confirmed.
    Confirmed { reason: Option<String> },
    /// User filled the form with `confirmed: false`.
    Refused { reason: Option<String> },
    /// User clicked decline on the dialog.
    Declined,
    /// User dismissed/cancelled the dialog.
    Cancelled,
    /// Client does not advertise elicitation capability.
    NotSupported,
    /// Protocol-level error (schema parse, transport, etc).
    Error(String),
}

impl ConfirmOutcome {
    /// Returns true only when the user actively confirmed the action.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, ConfirmOutcome::Confirmed { .. })
    }

    /// Short machine-readable tag for logging/responses.
    pub fn tag(&self) -> &'static str {
        match self {
            ConfirmOutcome::Confirmed { .. } => "confirmed",
            ConfirmOutcome::Refused { .. } => "refused",
            ConfirmOutcome::Declined => "declined",
            ConfirmOutcome::Cancelled => "cancelled",
            ConfirmOutcome::NotSupported => "not_supported",
            ConfirmOutcome::Error(_) => "error",
        }
    }
}

/// Ask the client to confirm a destructive or high-stakes action.
pub async fn confirm(peer: &Peer<RoleServer>, message: impl Into<String>) -> ConfirmOutcome {
    match peer.elicit::<UserConfirmation>(message).await {
        Ok(Some(c)) if c.confirmed => ConfirmOutcome::Confirmed { reason: c.reason },
        Ok(Some(c)) => ConfirmOutcome::Refused { reason: c.reason },
        Ok(None) => ConfirmOutcome::Cancelled,
        Err(ElicitationError::UserDeclined) => ConfirmOutcome::Declined,
        Err(ElicitationError::UserCancelled) => ConfirmOutcome::Cancelled,
        Err(ElicitationError::CapabilityNotSupported) => ConfirmOutcome::NotSupported,
        Err(ElicitationError::ParseError { error, .. }) => {
            ConfirmOutcome::Error(format!("elicitation parse error: {error}"))
        }
        Err(ElicitationError::NoContent) => ConfirmOutcome::Cancelled,
        Err(ElicitationError::Service(ServiceError::Timeout { .. })) => {
            ConfirmOutcome::Error("elicitation timeout".to_string())
        }
        Err(ElicitationError::Service(e)) => {
            ConfirmOutcome::Error(format!("elicitation service error: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_confirmation_roundtrips_json() {
        let c = UserConfirmation {
            confirmed: true,
            reason: Some("looks good".into()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: UserConfirmation = serde_json::from_str(&json).unwrap();
        assert!(back.confirmed);
        assert_eq!(back.reason.as_deref(), Some("looks good"));
    }

    #[test]
    fn user_confirmation_accepts_missing_reason() {
        let back: UserConfirmation = serde_json::from_str(r#"{"confirmed": false}"#).unwrap();
        assert!(!back.confirmed);
        assert!(back.reason.is_none());
    }

    #[test]
    fn tag_reflects_outcome() {
        assert_eq!(
            ConfirmOutcome::Confirmed { reason: None }.tag(),
            "confirmed"
        );
        assert_eq!(ConfirmOutcome::Refused { reason: None }.tag(), "refused");
        assert_eq!(ConfirmOutcome::Declined.tag(), "declined");
        assert_eq!(ConfirmOutcome::Cancelled.tag(), "cancelled");
        assert_eq!(ConfirmOutcome::NotSupported.tag(), "not_supported");
        assert_eq!(ConfirmOutcome::Error("x".into()).tag(), "error");
    }

    #[test]
    fn is_confirmed_only_for_confirmed_variant() {
        assert!(ConfirmOutcome::Confirmed { reason: None }.is_confirmed());
        assert!(!ConfirmOutcome::Refused { reason: None }.is_confirmed());
        assert!(!ConfirmOutcome::Declined.is_confirmed());
        assert!(!ConfirmOutcome::Cancelled.is_confirmed());
        assert!(!ConfirmOutcome::NotSupported.is_confirmed());
        assert!(!ConfirmOutcome::Error("e".into()).is_confirmed());
    }
}
