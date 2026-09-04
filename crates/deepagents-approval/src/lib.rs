//! Manual/Auto/YOLO, classifier, HITL checkpoint (Q18)
//!
//! This crate implements the approval subsystem described in
//! `docs/SPEC.md` §Q18. It provides three approval modes (Manual, Auto,
//! Yolo), a pluggable classifier that decides whether a dangerous
//! operation should be allowed, denied, or escalated to a human, and the
//! in-memory [`ApprovalState`] that tracks pending requests and the
//! consecutive-denial / consecutive-unavailable counters used to trigger
//! fallback behavior.
//!
//! Part of the deepagents-rust workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

pub use deepagents_errors::Error;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Number of consecutive denials after which the approval subsystem falls
/// back to requiring explicit human approval for everything.
pub const CONSECUTIVE_DENIAL_FALLBACK: u32 = 3;

/// Number of consecutive classifier-unavailable results after which the
/// approval subsystem falls back to requiring explicit human approval.
pub const CONSECUTIVE_UNAVAILABLE_FALLBACK: u32 = 2;

/// Placeholder v0 classifier policy prompt. Replaced with a richer
/// prompt once the classifier model integration lands.
pub const CLASSIFIER_POLICY_PROMPT: &str = "You are a tool-approval classifier. \
Given a tool invocation, decide whether to allow, deny, or require human review.";

// ── 1. ApprovalMode ───────────────────────────────────────────────────

/// The mode governing how tool approvals are handled.
///
/// - [`ApprovalMode::Manual`]: every potentially dangerous operation
///   requires explicit human approval.
/// - [`ApprovalMode::Auto`]: the classifier decides whether to allow,
///   deny, or require human review.
/// - [`ApprovalMode::Yolo`]: all operations are auto-approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Every dangerous operation needs a human in the loop.
    Manual,
    /// The classifier decides allow / deny / require_human.
    Auto,
    /// All operations are auto-approved (dangerous!).
    Yolo,
}

// ── 2. AutoDecisionCategory ───────────────────────────────────────────

/// The policy category assigned to an auto-decision by the classifier.
///
/// Categories are stable identifiers (not free text) so that policy
/// overrides and telemetry can key off them deterministically.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoDecisionCategory {
    /// The request escalates the agent's scope beyond the original task.
    ScopeEscalation,
    /// The request performs a destructive / irreversible action.
    DestructiveAction,
    /// The request accesses credentials or secrets.
    CredentialAccess,
    /// The request shares data externally.
    ExternalSharing,
    /// The request attempts to bypass a security control.
    SecurityBypass,
    /// The request modifies persistent state (e.g. installs, writes config).
    Persistence,
    /// The request touches a protected resource.
    ProtectedResource,
    /// The request crosses a trust boundary.
    TrustBoundary,
    /// The request triggers some other policy consideration.
    OtherPolicy,
}

// ── 3. DecisionDisposition ────────────────────────────────────────────

/// The final disposition of a classifier decision.
///
/// Combines deterministic (rule-based) results, classifier results, and
/// policy denials into a single enum that the approval loop can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionDisposition {
    /// Deterministically allowed (e.g. by a rule, not the classifier).
    DeterministicAllow,
    /// The classifier allowed the operation.
    ClassifierAllow,
    /// A policy rule denies the operation.
    PolicyDeny,
    /// The classifier was unavailable; the decision could not be made.
    ClassifierUnavailable,
    /// The operation must be escalated to a human.
    RequireHuman,
}

// ── 4. AutoDecision ───────────────────────────────────────────────────

/// A single classifier decision for one operation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AutoDecision {
    /// The policy category the classifier assigned to the operation.
    pub category: AutoDecisionCategory,
    /// The disposition the classifier reached.
    pub disposition: DecisionDisposition,
    /// Free-text reasoning from the classifier justifying the disposition.
    pub reasoning: String,
}

// ── 5. AutoDecisionBatch ──────────────────────────────────────────────

/// A batch of classifier decisions, with an overall disposition that
/// aggregates them for the caller.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AutoDecisionBatch {
    /// The individual decisions in this batch.
    pub decisions: Vec<AutoDecision>,
    /// The overall disposition aggregated across [`Self::decisions`].
    pub overall_disposition: DecisionDisposition,
}

// ── 6. ApprovalRequest ────────────────────────────────────────────────

/// A request for approval of a tool invocation, pending human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Unique identifier for this request.
    pub id: String,
    /// The name of the tool being invoked.
    pub tool_name: String,
    /// The arguments to the tool invocation, as a JSON value.
    pub tool_args: serde_json::Value,
    /// Optional human-readable context describing why the call is being made.
    pub context: Option<String>,
    /// Unix timestamp (seconds) when the request was created.
    pub created_at: i64,
}

// ── 7. ApprovalResponse ───────────────────────────────────────────────

/// A human's (or fallback's) response to an [`ApprovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalResponse {
    /// The request is approved as-is.
    Approve,
    /// The request is rejected; `reason` explains why.
    Reject {
        /// Human-readable explanation for the rejection.
        reason: String,
    },
    /// The request is approved with edited arguments.
    Edit {
        /// The replacement arguments for the tool invocation.
        new_args: serde_json::Value,
    },
}

// ── 8. ApprovalState ──────────────────────────────────────────────────

/// In-memory state tracking pending approvals, history, and fallback
/// counters.
#[derive(Debug, Clone)]
pub struct ApprovalState {
    /// The active approval mode.
    pub mode: ApprovalMode,
    /// Requests awaiting a response.
    pub pending: Vec<ApprovalRequest>,
    /// Completed (request, response) pairs, oldest first.
    pub history: Vec<(ApprovalRequest, ApprovalResponse)>,
    /// Consecutive rejections seen so far.
    pub consecutive_denials: u32,
    /// Consecutive classifier-unavailable results seen so far.
    pub consecutive_unavailable: u32,
}

impl ApprovalState {
    /// Create a new state for the given [`ApprovalMode`].
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode,
            pending: Vec::new(),
            history: Vec::new(),
            consecutive_denials: 0,
            consecutive_unavailable: 0,
        }
    }

    /// Create and enqueue a new [`ApprovalRequest`] for the given tool
    /// invocation, returning a clone of the created request.
    pub fn create_request(
        &mut self,
        tool_name: impl Into<String>,
        tool_args: serde_json::Value,
    ) -> ApprovalRequest {
        let request = ApprovalRequest {
            id: format!("req_{}", self.history.len() + self.pending.len() + 1),
            tool_name: tool_name.into(),
            tool_args,
            context: None,
            created_at: 0,
        };
        self.pending.push(request.clone());
        request
    }

    /// Submit a response for the pending request with the given id.
    ///
    /// Moves the request from `pending` to `history` and updates the
    /// consecutive-denial / consecutive-unavailable counters. Returns
    /// [`deepagents_errors::Error`] if no matching pending request exists.
    pub fn submit_response(
        &mut self,
        request_id: &str,
        response: ApprovalResponse,
    ) -> Result<(), Error> {
        let position = self
            .pending
            .iter()
            .position(|r| r.id == request_id)
            .ok_or_else(|| {
                Error::Approval(
                    deepagents_errors::ApprovalError::HitlIterationLimit(format!(
                        "no pending request with id {request_id}"
                    )),
                )
            })?;
        let request = self.pending.remove(position);

        match &response {
            ApprovalResponse::Approve | ApprovalResponse::Edit { .. } => {
                self.consecutive_denials = 0;
                self.consecutive_unavailable = 0;
            }
            ApprovalResponse::Reject { .. } => {
                self.consecutive_denials = self.consecutive_denials.saturating_add(1);
                self.consecutive_unavailable = 0;
            }
        }

        self.history.push((request, response));
        Ok(())
    }

    /// Returns the currently pending (unanswered) requests.
    pub fn pending_requests(&self) -> &[ApprovalRequest] {
        &self.pending
    }

    /// Returns `true` when the consecutive-denial or consecutive-unavailable
    /// counters have crossed their fallback thresholds and the subsystem
    /// should fall back to manual approval.
    pub fn should_fallback(&self) -> bool {
        self.consecutive_denials >= CONSECUTIVE_DENIAL_FALLBACK
            || self.consecutive_unavailable >= CONSECUTIVE_UNAVAILABLE_FALLBACK
    }
}

// ── 9. ClassifierConfig ────────────────────────────────────────────────

/// Configuration for the [`Classifier`].
#[derive(Debug, Clone)]
pub struct ClassifierConfig {
    /// Deadline (in milliseconds) the classifier must respond within.
    pub deadline_ms: u64,
    /// Whether to fall back to manual approval when the classifier fails.
    pub fallback_enabled: bool,
    /// The policy prompt supplied to the classifier model.
    pub prompt: String,
}

impl ClassifierConfig {
    /// Create a default config (5000ms deadline, fallback enabled,
    /// placeholder v0 prompt).
    pub fn new() -> Self {
        Self {
            deadline_ms: 5000,
            fallback_enabled: true,
            prompt: CLASSIFIER_POLICY_PROMPT.to_string(),
        }
    }

    /// Builder-style setter for [`Self::deadline_ms`].
    pub fn with_deadline(mut self, ms: u64) -> Self {
        self.deadline_ms = ms;
        self
    }
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ── 10. Classifier ────────────────────────────────────────────────────

/// The approval classifier. v0 is a stub that always escalates to a
/// human; a later revision will wire it to a `rig` model.
#[derive(Debug, Clone)]
pub struct Classifier {
    /// The classifier configuration.
    pub config: ClassifierConfig,
}

impl Classifier {
    /// Create a new classifier with the given configuration.
    pub fn new(config: ClassifierConfig) -> Self {
        Self { config }
    }

    /// Classify an [`ApprovalRequest`].
    ///
    /// The v0 implementation is a stub: it always returns a single
    /// [`AutoDecision`] with disposition [`DecisionDisposition::RequireHuman`]
    /// and category [`AutoDecisionCategory::OtherPolicy`]. A later revision
    /// will call the configured model within [`ClassifierConfig::deadline_ms`]
    /// and fall back per [`ClassifierConfig::fallback_enabled`].
    pub async fn classify(&self, request: &ApprovalRequest) -> Result<AutoDecisionBatch, Error> {
        // v0 stub: always require human review.
        let decision = AutoDecision {
            category: AutoDecisionCategory::OtherPolicy,
            disposition: DecisionDisposition::RequireHuman,
            reasoning: format!(
                "v0 stub: escalating invocation of `{}` to human review",
                request.tool_name
            ),
        };
        Ok(AutoDecisionBatch {
            decisions: vec![decision],
            overall_disposition: DecisionDisposition::RequireHuman,
        })
    }

    /// Parse a fallback JSON document into an [`AutoDecisionBatch`].
    ///
    /// Used when the classifier is unavailable and a cached / canned
    /// decision must be loaded from disk.
    pub fn parse_fallback_json(json: &str) -> Result<AutoDecisionBatch, Error> {
        let batch: AutoDecisionBatch = serde_json::from_str(json)?;
        Ok(batch)
    }
}

// ── 11. BypassTier ────────────────────────────────────────────────────

/// The "YOLO" bypass tier currently in effect.
///
/// Higher tiers bypass progressively more safeguards. The default /
/// no-bypass tier is [`BypassTier::None`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BypassTier {
    /// No bypass; all safeguards active.
    None,
    /// Auto-approve everything (equivalent to [`ApprovalMode::Yolo`]).
    AutoApprove,
    /// Bypass hook execution.
    BypassHooks,
    /// Bypass the approval subsystem entirely.
    BypassApproval,
    /// Bypass everything (hooks + approval + sandbox).
    BypassAll,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approval_mode_serde() {
        let mode = ApprovalMode::Auto;
        let json = serde_json::to_string(&mode).expect("serialize");
        assert_eq!(json, "\"auto\"");
        let back: ApprovalMode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, mode);

        assert_eq!(serde_json::to_string(&ApprovalMode::Manual).unwrap(), "\"manual\"");
        assert_eq!(serde_json::to_string(&ApprovalMode::Yolo).unwrap(), "\"yolo\"");
    }

    #[test]
    fn test_auto_decision_category_serde() {
        let cat = AutoDecisionCategory::SecurityBypass;
        let json = serde_json::to_string(&cat).expect("serialize");
        assert_eq!(json, "\"security_bypass\"");
        let back: AutoDecisionCategory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, cat);

        let all = vec![
            AutoDecisionCategory::ScopeEscalation,
            AutoDecisionCategory::DestructiveAction,
            AutoDecisionCategory::CredentialAccess,
            AutoDecisionCategory::ExternalSharing,
            AutoDecisionCategory::SecurityBypass,
            AutoDecisionCategory::Persistence,
            AutoDecisionCategory::ProtectedResource,
            AutoDecisionCategory::TrustBoundary,
            AutoDecisionCategory::OtherPolicy,
        ];
        for c in all {
            let json = serde_json::to_string(&c).unwrap();
            let back: AutoDecisionCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, c);
        }
        // JsonSchema is derivable (does not panic).
        let _ = schemars::schema_for!(AutoDecisionCategory);
    }

    #[test]
    fn test_decision_disposition_serde() {
        let cases = [
            (DecisionDisposition::DeterministicAllow, "deterministic_allow"),
            (DecisionDisposition::ClassifierAllow, "classifier_allow"),
            (DecisionDisposition::PolicyDeny, "policy_deny"),
            (DecisionDisposition::ClassifierUnavailable, "classifier_unavailable"),
            (DecisionDisposition::RequireHuman, "require_human"),
        ];
        for (disp, expected) in cases {
            let json = serde_json::to_string(&disp).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: DecisionDisposition = serde_json::from_str(&json).unwrap();
            assert_eq!(back, disp);
        }
        let _ = schemars::schema_for!(DecisionDisposition);
    }

    #[test]
    fn test_approval_state_create_submit() {
        let mut state = ApprovalState::new(ApprovalMode::Manual);
        let req = state.create_request("shell", serde_json::json!({"cmd": "rm -rf /"}));
        assert_eq!(state.pending_requests().len(), 1);
        assert_eq!(state.pending_requests()[0].id, req.id);

        state
            .submit_response(&req.id, ApprovalResponse::Approve)
            .expect("submit");
        assert!(state.pending_requests().is_empty());
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.consecutive_denials, 0);

        // Reject path increments counters.
        let req2 = state.create_request("shell", serde_json::json!({"cmd": "rm -rf /tmp"}));
        state
            .submit_response(&req2.id, ApprovalResponse::Reject { reason: "no".into() })
            .expect("submit reject");
        assert_eq!(state.consecutive_denials, 1);

        // Unknown id errors.
        let err = state.submit_response("nope", ApprovalResponse::Approve);
        assert!(err.is_err());
    }

    #[test]
    fn test_approval_state_should_fallback() {
        let mut state = ApprovalState::new(ApprovalMode::Auto);
        assert!(!state.should_fallback());

        // Three consecutive denials -> fallback.
        for i in 0..3 {
            let req = state.create_request("shell", serde_json::json!({"i": i}));
            state
                .submit_response(&req.id, ApprovalResponse::Reject { reason: "no".into() })
                .unwrap();
        }
        assert!(state.should_fallback());

        // Reset and test unavailable path (simulate by directly setting).
        let mut state = ApprovalState::new(ApprovalMode::Auto);
        state.consecutive_unavailable = 2;
        assert!(state.should_fallback());

        state.consecutive_unavailable = 1;
        assert!(!state.should_fallback());

        // Below threshold.
        let mut state = ApprovalState::new(ApprovalMode::Auto);
        for i in 0..2 {
            let req = state.create_request("shell", serde_json::json!({"i": i}));
            state
                .submit_response(&req.id, ApprovalResponse::Reject { reason: "no".into() })
                .unwrap();
        }
        assert!(!state.should_fallback());
    }

    #[test]
    fn test_classifier_parse_fallback_json() {
        let json = r#"{
            "decisions": [
                {
                    "category": "destructive_action",
                    "disposition": "policy_deny",
                    "reasoning": "rm is destructive"
                }
            ],
            "overall_disposition": "policy_deny"
        }"#;
        let batch = Classifier::parse_fallback_json(json).expect("parse");
        assert_eq!(batch.decisions.len(), 1);
        assert_eq!(batch.decisions[0].category, AutoDecisionCategory::DestructiveAction);
        assert_eq!(batch.decisions[0].disposition, DecisionDisposition::PolicyDeny);
        assert_eq!(batch.overall_disposition, DecisionDisposition::PolicyDeny);
    }

    #[test]
    fn test_bypass_tier_serde() {
        let cases = [
            (BypassTier::None, "none"),
            (BypassTier::AutoApprove, "auto_approve"),
            (BypassTier::BypassHooks, "bypass_hooks"),
            (BypassTier::BypassApproval, "bypass_approval"),
            (BypassTier::BypassAll, "bypass_all"),
        ];
        for (tier, expected) in cases {
            let json = serde_json::to_string(&tier).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: BypassTier = serde_json::from_str(&json).unwrap();
            assert_eq!(back, tier);
        }
    }

    #[tokio::test]
    async fn test_classifier_stub_returns_require_human() {
        let classifier = Classifier::new(ClassifierConfig::new());
        let req = ApprovalRequest {
            id: "test".into(),
            tool_name: "shell".into(),
            tool_args: serde_json::json!({}),
            context: None,
            created_at: 0,
        };
        let batch = classifier.classify(&req).await.expect("classify");
        assert_eq!(batch.overall_disposition, DecisionDisposition::RequireHuman);
    }
}
