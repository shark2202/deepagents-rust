//! Human-in-the-Loop (HITL): interrupt policy, pause + resume (Q4).
//!
//! The HITL middleware intercepts tool calls that require human approval
//! before execution. It implements [`AgentHook`] and checks each tool call
//! against an [`InterruptPolicy`] map. When a tool requires human approval,
//! the run is paused via [`ToolCallAction::Stop`], the [`AgentRun`] state is
//! serialized as a checkpoint, and the caller can later deserialize, feed
//! back the human decision, and resume.
//!
//! See `docs/SPEC.md` §Q4 for the design rationale.

use std::collections::HashMap;
use std::sync::Arc;

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, CompletionResponseEvent, HookContext,
    InvalidToolCallAction, ModelSelection, ModelSelectionAction,
    ModelTurnAction, ModelTurnFinished, ObservationAction, StreamResponseFinish, TextDelta,
    ReasoningDelta, ToolCallDelta, ToolCallAction, ToolCall as RigToolCall, ToolResultAction,
    ToolResultEvent,
};
use rig_core::wasm_compat::WasmCompatSend;

use serde::{Deserialize, Serialize};

// ── Approval decision ──────────────────────────────────────────────────

/// The outcome of a human or automated approval check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// The tool call is approved — execute it.
    Approve,
    /// The tool call is rejected — return feedback to the model.
    Reject(String),
    /// A human must decide — pause the run.
    RequireHuman,
}

impl Default for ApprovalDecision {
    fn default() -> Self {
        Self::RequireHuman
    }
}

// ── Tool call info (owned, for resolvers) ──────────────────────────────

/// Owned snapshot of a tool call, passed to [`Resolver`] functions.
///
/// This is the owned counterpart of rig's borrowed [`ToolCall<'a>`],
/// allowing resolver closures to work with owned data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    /// Tool name.
    pub tool_name: String,
    /// Durable tool-call id (provider's when available, else rig's).
    pub tool_call_id: Option<String>,
    /// Rig correlation id.
    pub internal_call_id: String,
    /// Effective JSON arguments (as a string).
    pub args: String,
}

impl<'a> From<RigToolCall<'a>> for ToolCallInfo {
    fn from(tc: RigToolCall<'a>) -> Self {
        Self {
            tool_name: tc.tool_name.to_string(),
            tool_call_id: tc.tool_call_id.map(|s| s.to_string()),
            internal_call_id: tc.internal_call_id.to_string(),
            args: tc.args.to_string(),
        }
    }
}

// ── Resolver ───────────────────────────────────────────────────────────

/// A function that resolves an approval decision for a tool call.
///
/// Resolvers are used by [`InterruptPolicy::WithResolver`] to auto-approve
/// whitelisted paths and force approval for dangerous operations.
pub type Resolver = Arc<dyn Fn(&ToolCallInfo) -> ApprovalDecision + Send + Sync>;

// ── Interrupt policy ───────────────────────────────────────────────────

/// Policy for whether a tool call should be interrupted for human approval.
///
/// Maps to the original Python `interrupt_on` dict values:
/// - `True` → [`InterruptPolicy::Simple(true)`] (always require human)
/// - `False` → [`InterruptPolicy::Simple(false)`] (never interrupt)
/// - `{"resolver": ...}` → [`InterruptPolicy::WithResolver`]
#[derive(Clone)]
pub enum InterruptPolicy {
    /// Simple binary: `true` = always require human approval, `false` = never.
    Simple(bool),
    /// Use a resolver function to auto-approve/reject or require human.
    WithResolver {
        /// The resolver closure.
        auto: Resolver,
    },
}

impl InterruptPolicy {
    /// Create a simple policy that always requires human approval.
    pub fn require_human() -> Self {
        Self::Simple(true)
    }

    /// Create a simple policy that never interrupts.
    pub fn never() -> Self {
        Self::Simple(false)
    }

    /// Create a policy with a custom resolver.
    pub fn with_resolver(auto: Resolver) -> Self {
        Self::WithResolver { auto }
    }

    /// Evaluate the policy for a given tool call, returning the decision.
    pub fn evaluate(&self, info: &ToolCallInfo) -> ApprovalDecision {
        match self {
            Self::Simple(true) => ApprovalDecision::RequireHuman,
            Self::Simple(false) => ApprovalDecision::Approve,
            Self::WithResolver { auto } => (auto)(info),
        }
    }

    /// Returns `true` if this policy never interrupts.
    pub fn is_never(&self) -> bool {
        matches!(self, Self::Simple(false))
    }

    /// Returns `true` if this policy always requires human approval.
    pub fn always_requires_human(&self) -> bool {
        matches!(self, Self::Simple(true))
    }
}

impl std::fmt::Debug for InterruptPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simple(b) => f.debug_tuple("Simple").field(b).finish(),
            Self::WithResolver { .. } => f.debug_struct("WithResolver").finish_non_exhaustive(),
        }
    }
}

impl Default for InterruptPolicy {
    fn default() -> Self {
        Self::Simple(false)
    }
}

impl<'de> Deserialize<'de> for InterruptPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Visitor for deserializing `InterruptPolicy` from a simple bool
        /// or a flag string. Resolver-based policies cannot be deserialized
        /// (closures are not serializable); they must be re-attached at
        /// runtime via [`InterruptPolicy::with_resolver`].
        struct PolicyVisitor;

        impl<'de> serde::de::Visitor<'de> for PolicyVisitor {
            type Value = InterruptPolicy;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a boolean or \"require_human\"")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(InterruptPolicy::Simple(v))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match v {
                    "require_human" | "true" => Ok(InterruptPolicy::Simple(true)),
                    "never" | "false" => Ok(InterruptPolicy::Simple(false)),
                    _ => Err(serde::de::Error::custom(format!(
                        "unknown interrupt policy: {v}"
                    ))),
                }
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(InterruptPolicy::Simple(true))
            }
        }

        deserializer.deserialize_any(PolicyVisitor)
    }
}

impl Serialize for InterruptPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            InterruptPolicy::Simple(b) => serializer.serialize_bool(*b),
            InterruptPolicy::WithResolver { .. } => {
                // Resolvers are not serializable; serialize as require_human
                serializer.serialize_bool(true)
            }
        }
    }
}

// ── Interrupt map ──────────────────────────────────────────────────────

/// A map of tool names to their interrupt policies.
///
/// This is the Rust equivalent of the original Python `interrupt_on` dict.
/// Tool names not in the map are never interrupted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterruptMap {
    /// Inner map: tool name → policy.
    pub policies: HashMap<String, InterruptPolicy>,
}

impl InterruptMap {
    /// Create an empty interrupt map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from a `HashMap<String, InterruptPolicy>`.
    pub fn from_map(policies: HashMap<String, InterruptPolicy>) -> Self {
        Self { policies }
    }

    /// Insert a policy for a tool name.
    pub fn insert(&mut self, tool_name: impl Into<String>, policy: InterruptPolicy) {
        self.policies.insert(tool_name.into(), policy);
    }

    /// Insert a simple boolean policy for a tool name.
    pub fn insert_simple(&mut self, tool_name: impl Into<String>, require_human: bool) {
        self.policies
            .insert(tool_name.into(), InterruptPolicy::Simple(require_human));
    }

    /// Insert a resolver-based policy for a tool name.
    pub fn insert_resolver(&mut self, tool_name: impl Into<String>, resolver: Resolver) {
        self.policies
            .insert(tool_name.into(), InterruptPolicy::with_resolver(resolver));
    }

    /// Look up the policy for a tool name. Returns `None` if not registered.
    pub fn get(&self, tool_name: &str) -> Option<&InterruptPolicy> {
        self.policies.get(tool_name)
    }

    /// Check if a tool call should be interrupted, and return the decision.
    ///
    /// Tools not in the map are auto-approved.
    pub fn check(&self, info: &ToolCallInfo) -> ApprovalDecision {
        match self.policies.get(&info.tool_name) {
            Some(policy) => policy.evaluate(info),
            None => ApprovalDecision::Approve,
        }
    }

    /// Returns `true` if the map is empty (no tools are interrupted).
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }
}

// ── HITL middleware hook ───────────────────────────────────────────────

/// The HITL middleware: an [`AgentHook`] that intercepts tool calls
/// requiring human approval.
///
/// When a tool call matches a policy that requires human approval, the hook
/// returns [`ToolCallAction::Stop("awaiting approval")`], pausing the run.
/// The caller serializes the [`AgentRun`] as a checkpoint, collects the
/// human decision, then deserializes and resumes.
///
/// For resolver-based policies, the resolver is called synchronously. If it
/// returns [`ApprovalDecision::Approve`], the tool executes normally. If it
/// returns [`ApprovalDecision::Reject`], the tool is skipped with feedback.
/// If it returns [`ApprovalDecision::RequireHuman`], the run pauses.
pub struct HitlMiddleware {
    /// The interrupt map (tool name → policy).
    interrupt_on: InterruptMap,
}

impl HitlMiddleware {
    /// Create a new HITL middleware with the given interrupt map.
    pub fn new(interrupt_on: InterruptMap) -> Self {
        Self { interrupt_on }
    }

    /// Create an empty HITL middleware (no interruptions).
    pub fn empty() -> Self {
        Self::new(InterruptMap::new())
    }

    /// Get a reference to the interrupt map.
    pub fn interrupt_on(&self) -> &InterruptMap {
        &self.interrupt_on
    }
}

impl std::fmt::Debug for HitlMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HitlMiddleware")
            .field("interrupt_on", &self.interrupt_on)
            .finish()
    }
}

impl Default for HitlMiddleware {
    fn default() -> Self {
        Self::empty()
    }
}

impl AgentHook for HitlMiddleware {
    fn on_model_select(
        &self,
        _ctx: &HookContext,
        _event: ModelSelection<'_>,
    ) -> ModelSelectionAction {
        ModelSelectionAction::Continue
    }

    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> impl std::future::Future<Output = CompletionCallAction> + WasmCompatSend {
        async { CompletionCallAction::Continue }
    }

    fn on_completion_response(
        &self,
        _ctx: &HookContext,
        _event: CompletionResponseEvent<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        _event: ModelTurnFinished<'_>,
    ) -> impl std::future::Future<Output = ModelTurnAction> + WasmCompatSend {
        async { ModelTurnAction::Continue }
    }

    fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        _event: &rig_agent::agent::InvalidToolCallContext,
    ) -> impl std::future::Future<Output = Option<InvalidToolCallAction>> + WasmCompatSend
    {
        async { None }
    }

    fn on_tool_call(
        &self,
        _ctx: &HookContext,
        event: RigToolCall<'_>,
    ) -> impl std::future::Future<Output = ToolCallAction> + WasmCompatSend {
        let info = ToolCallInfo::from(event);
        let decision = self.interrupt_on.check(&info);

        async move {
            match decision {
                ApprovalDecision::Approve => ToolCallAction::Run,
                ApprovalDecision::Reject(reason) => ToolCallAction::skip(reason),
                ApprovalDecision::RequireHuman => {
                    ToolCallAction::stop("awaiting approval")
                }
            }
        }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        _event: ToolResultEvent<'_>,
    ) -> impl std::future::Future<Output = ToolResultAction> + WasmCompatSend {
        async { ToolResultAction::Keep }
    }

    fn on_text_delta(
        &self,
        _ctx: &HookContext,
        _event: TextDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_reasoning_delta(
        &self,
        _ctx: &HookContext,
        _event: ReasoningDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_stream_response_finish(
        &self,
        _ctx: &HookContext,
        _event: StreamResponseFinish<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }
}

// ── Pause + Resume checkpoint ──────────────────────────────────────────

/// A serialized checkpoint of a paused agent run.
///
/// This wraps the serialized [`AgentRun`] state along with metadata about
/// which tool call triggered the pause. The caller deserializes this,
/// collects a human decision, then feeds it back via [`ResumeInput`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseCheckpoint {
    /// The serialized AgentRun state (opaque JSON).
    pub run_state: serde_json::Value,
    /// The tool call that triggered the pause.
    pub pending_tool_call: ToolCallInfo,
    /// The run id (if available).
    pub run_id: Option<String>,
    /// The turn number when the pause occurred.
    pub turn: usize,
}

/// The human's decision fed back to resume a paused run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResumeInput {
    /// Approve the tool call — execute it.
    Approve,
    /// Reject the tool call with feedback to the model.
    Reject(String),
    /// Edit the tool arguments before execution.
    Edit {
        /// New JSON arguments for the tool call.
        new_args: serde_json::Value,
    },
}

impl ResumeInput {
    /// Create an approve input.
    pub fn approve() -> Self {
        Self::Approve
    }

    /// Create a reject input with feedback.
    pub fn reject(reason: impl Into<String>) -> Self {
        Self::Reject(reason.into())
    }

    /// Create an edit input with new arguments.
    pub fn edit(new_args: serde_json::Value) -> Self {
        Self::Edit { new_args }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_policy_approve() {
        let policy = InterruptPolicy::Simple(false);
        let info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(policy.evaluate(&info), ApprovalDecision::Approve);
    }

    #[test]
    fn test_simple_policy_require_human() {
        let policy = InterruptPolicy::Simple(true);
        let info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(policy.evaluate(&info), ApprovalDecision::RequireHuman);
    }

    #[test]
    fn test_resolver_policy_approve() {
        let resolver: Resolver = Arc::new(|_info| ApprovalDecision::Approve);
        let policy = InterruptPolicy::with_resolver(resolver);
        let info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(policy.evaluate(&info), ApprovalDecision::Approve);
    }

    #[test]
    fn test_resolver_policy_reject() {
        let resolver: Resolver =
            Arc::new(|_info| ApprovalDecision::Reject("not allowed".into()));
        let policy = InterruptPolicy::with_resolver(resolver);
        let info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        match policy.evaluate(&info) {
            ApprovalDecision::Reject(msg) => assert_eq!(msg, "not allowed"),
            _ => panic!("expected Reject"),
        }
    }

    #[test]
    fn test_interrupt_map_no_entry_approves() {
        let map = InterruptMap::new();
        let info = ToolCallInfo {
            tool_name: "read_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(map.check(&info), ApprovalDecision::Approve);
    }

    #[test]
    fn test_interrupt_map_require_human() {
        let mut map = InterruptMap::new();
        map.insert_simple("write_file", true);
        let info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(map.check(&info), ApprovalDecision::RequireHuman);
    }

    #[test]
    fn test_interrupt_map_resolver() {
        let mut map = InterruptMap::new();
        let resolver: Resolver = Arc::new(|info| {
            if info.tool_name == "write_file" {
                ApprovalDecision::RequireHuman
            } else {
                ApprovalDecision::Approve
            }
        });
        map.insert_resolver("write_file", resolver);

        let write_info = ToolCallInfo {
            tool_name: "write_file".into(),
            tool_call_id: None,
            internal_call_id: "1".into(),
            args: "{}".into(),
        };
        assert_eq!(
            map.check(&write_info),
            ApprovalDecision::RequireHuman
        );

        let read_info = ToolCallInfo {
            tool_name: "read_file".into(),
            tool_call_id: None,
            internal_call_id: "2".into(),
            args: "{}".into(),
        };
        assert_eq!(map.check(&read_info), ApprovalDecision::Approve);
    }

    #[test]
    fn test_policy_serialize_deserialize() {
        let policy = InterruptPolicy::Simple(true);
        let json = serde_json::to_string(&policy).unwrap();
        assert_eq!(json, "true");

        let deserialized: InterruptPolicy = serde_json::from_str("false").unwrap();
        assert!(matches!(deserialized, InterruptPolicy::Simple(false)));
    }

    #[test]
    fn test_resume_input_constructors() {
        let approve = ResumeInput::approve();
        assert!(matches!(approve, ResumeInput::Approve));

        let reject = ResumeInput::reject("bad idea");
        assert!(matches!(reject, ResumeInput::Reject(_)));

        let edit = ResumeInput::edit(serde_json::json!({"path": "/new"}));
        assert!(matches!(edit, ResumeInput::Edit { .. }));
    }
}
