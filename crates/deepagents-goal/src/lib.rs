//! GoalStatus, GraderResponse, self-grading loop (Q23).
//!
//! This crate implements the *goal/rubric self-grading loop* described in the
//! LangChain Deep Agents design specification (SPEC Q23). A goal carries an
//! objective plus a set of success criteria; an agent executes against the
//! goal, a grader evaluates the result, and the loop either terminates or
//! injects revision feedback until the goal is satisfied, fails, or the
//! maximum iteration count is reached.
//!
//! The crate is intentionally side-effect free at this layer: it owns the
//! data model, the verdict-application state machine, and a simple isolated
//! budget tracker. The actual grader invocation (model call) is driven by a
//! higher layer that calls [`SelfGradingLoop::process_grader_response`].
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` §七 for the full architecture.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use deepagents_errors::Error;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum total character length of a goal's `objective` plus its
/// `criteria` strings combined.
///
/// A goal whose combined objective+criteria text exceeds this limit is
/// rejected by [`GoalSpec`] validation.
pub const GOAL_APPLICATION_CHAR_LIMIT: usize = 8_000;

/// Maximum character length of a single status note / feedback string.
///
/// Feedback injected into a goal's state via
/// [`GoalState::add_feedback`] must not exceed this limit, otherwise it is
/// rejected with a [`GoalError::StateSize`] wrapped in [`Error::Goal`].
pub const GOAL_STATUS_NOTE_CHAR_LIMIT: usize = 4_000;

/// Default maximum number of grader iterations before the self-grading loop
/// gives up with [`RubricResult::MaxIterationsReached`].
pub const MAX_ITERATIONS: u32 = 3;

/// Sentinel string used in a rubric model spec to indicate that the rubric
/// should inherit the parent agent's model rather than using a dedicated one.
///
/// The `_rubric_model_spec` field of [`RubricConfig`] is a 3-state value:
/// `None` (inherit parent), `Some(INHERIT_RUBRIC_MODEL)` (also inherit
/// parent, set explicitly), or `Some("<model spec>")` (use a dedicated model).
pub const INHERIT_RUBRIC_MODEL: &str = "INHERIT";

// ── 1. GoalStatus ─────────────────────────────────────────────────────

/// Lifecycle status of a [`GoalState`].
///
/// Maps to the four statuses defined in SPEC Q23 (`active` / `paused` /
/// `blocked` / `complete`). The serde representation uses the lower-cased
/// snake-case form to match the wire format expected by the SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// The goal is active and the agent may make progress on it.
    Active,
    /// The goal is paused; the agent should not currently work on it.
    Paused,
    /// The goal is blocked by an external dependency that must be resolved.
    Blocked,
    /// The goal is complete (satisfied or definitively failed).
    Complete,
}

// ── 2. GraderVerdict ───────────────────────────────────────────────────

/// Top-level verdict returned by the grader for a single grading pass.
///
/// Drives the self-grading loop's state transitions in
/// [`SelfGradingLoop::process_grader_response`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraderVerdict {
    /// The goal's criteria are satisfied; the loop should terminate.
    Satisfied,
    /// The goal needs revision; feedback should be injected and the agent
    /// should continue.
    NeedsRevision,
    /// The goal has definitively failed; the loop should terminate.
    Failed,
}

// ── 3. CriterionEval ──────────────────────────────────────────────────

/// Per-criterion evaluation result emitted by the grader.
///
/// Each entry maps to one of the criteria strings in [`GoalSpec::criteria`]
/// and either passes it or describes the gap that prevented a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CriterionEval {
    /// The criterion named `name` was satisfied.
    Pass {
        /// Name of the satisfied criterion.
        name: String,
    },
    /// The criterion named `name` was not satisfied; `gap` explains why.
    Fail {
        /// Name of the failed criterion.
        name: String,
        /// Human-readable description of what is missing / wrong.
        gap: String,
    },
}

// ── 4. GraderResponse ─────────────────────────────────────────────────

/// Structured output returned by the grader for a single evaluation pass.
///
/// Produced by the grader model call and consumed by
/// [`SelfGradingLoop::process_grader_response`]. Derives
/// [`schemars::JsonSchema`] so it can be used as a structured-output schema
/// for the underlying model runtime (`rig`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraderResponse {
    /// Top-level verdict for this grading pass.
    pub result: GraderVerdict,
    /// Free-text explanation from the grader justifying the verdict.
    pub explanation: String,
    /// Per-criterion evaluation results, one per criterion in the goal spec.
    pub criteria: Vec<CriterionEval>,
}

// ── 5. RubricResult ───────────────────────────────────────────────────

/// Terminal outcome of the self-grading loop.
///
/// Returned by [`SelfGradingLoop::process_grader_response`] once a pass has
/// been applied. `Satisfied`, `Failed`, and `MaxIterationsReached` are all
/// terminal "done" states; `NeedsRevision` indicates the loop should
/// continue after feedback injection. `GraderError` is also terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RubricResult {
    /// The goal is satisfied; the loop is done.
    Satisfied,
    /// The goal needs revision; inject feedback and continue.
    NeedsRevision,
    /// The goal has definitively failed; the loop is done.
    Failed,
    /// The loop exceeded [`RubricConfig::max_iterations`] without
    /// satisfaction; the loop is done.
    MaxIterationsReached,
    /// The grader itself produced an error; the loop is done. The inner
    /// string carries the error detail.
    GraderError(String),
}

// ── 6. GoalSpec ───────────────────────────────────────────────────────

/// A goal specification: an objective plus a list of success criteria.
///
/// The combined character length of `objective` plus every `criteria` string
/// must not exceed [`GOAL_APPLICATION_CHAR_LIMIT`]; this is enforced by
/// [`GoalSpec::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSpec {
    /// Human-readable description of the goal's objective.
    pub objective: String,
    /// Success criteria the grader checks against.
    pub criteria: Vec<String>,
}

impl GoalSpec {
    /// Construct a new `GoalSpec` from an objective and criteria, validating
    /// the combined character length against [`GOAL_APPLICATION_CHAR_LIMIT`].
    ///
    /// Returns [`Error::Goal`] with [`GoalError::StateSize`] if the
    /// combined length exceeds the limit.
    pub fn new(objective: String, criteria: Vec<String>) -> Result<Self, Error> {
        let spec = Self { objective, criteria };
        spec.validate()?;
        Ok(spec)
    }

    /// Compute the total character length of `objective` plus all
    /// `criteria` strings combined.
    pub fn application_char_length(&self) -> usize {
        self.objective.chars().count()
            + self.criteria.iter().map(|c| c.chars().count()).sum::<usize>()
    }

    /// Validate that the combined objective+criteria length fits within
    /// [`GOAL_APPLICATION_CHAR_LIMIT`].
    pub fn validate(&self) -> Result<(), Error> {
        let len = self.application_char_length();
        if len > GOAL_APPLICATION_CHAR_LIMIT {
            return Err(Error::Goal(
                deepagents_errors::GoalError::StateSize(format!(
                    "goal application text is {len} chars, limit is \
                     {GOAL_APPLICATION_CHAR_LIMIT}"
                )),
            ));
        }
        Ok(())
    }
}

// ── 7. GoalState ──────────────────────────────────────────────────────

/// Runtime state for a single goal, tracked across self-grading iterations.
///
/// Carries the spec, lifecycle status, iteration counter, accumulated
/// feedback notes, and an optional status note. Created via [`GoalState::new`]
/// and mutated by [`SelfGradingLoop::process_grader_response`] and friends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    /// Unique identifier for this goal instance.
    pub id: uuid::Uuid,
    /// The goal specification (objective + criteria).
    pub spec: GoalSpec,
    /// Current lifecycle status.
    pub status: GoalStatus,
    /// Number of grader iterations completed so far.
    pub iterations: u32,
    /// Accumulated revision feedback notes (latest is last).
    pub feedback: Vec<String>,
    /// Optional human-readable status note.
    pub status_note: Option<String>,
}

impl GoalState {
    /// Construct a new active goal state with zero iterations and no
    /// feedback, freshly assigned a UUID.
    pub fn new(spec: GoalSpec) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            spec,
            status: GoalStatus::Active,
            iterations: 0,
            feedback: Vec::new(),
            status_note: None,
        }
    }

    /// Append a feedback note to the goal state.
    ///
    /// The note's character length must not exceed
    /// [`GOAL_STATUS_NOTE_CHAR_LIMIT`]; otherwise it is rejected with
    /// [`Error::Goal`] wrapping [`GoalError::StateSize`].
    pub fn add_feedback(&mut self, feedback: String) -> Result<(), Error> {
        let len = feedback.chars().count();
        if len > GOAL_STATUS_NOTE_CHAR_LIMIT {
            return Err(Error::Goal(
                deepagents_errors::GoalError::StateSize(format!(
                    "feedback is {len} chars, limit is \
                     {GOAL_STATUS_NOTE_CHAR_LIMIT}"
                )),
            ));
        }
        self.feedback.push(feedback);
        Ok(())
    }

    /// Increment the iteration counter by one.
    pub fn advance_iteration(&mut self) {
        self.iterations += 1;
    }
}

// ── 8. RubricConfig ───────────────────────────────────────────────────

/// Configuration for the self-grading rubric loop.
///
/// The `rubric_model_spec` / `inherit_model` pair encodes the 3-state
/// `_rubric_model_spec` described in SPEC Q23: absent (inherit parent),
/// [`INHERIT_RUBRIC_MODEL`] (inherit parent, set explicitly), or a concrete
/// model spec string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubricConfig {
    /// Maximum number of grader iterations before giving up.
    pub max_iterations: u32,
    /// Optional rubric model spec. If `None` or equal to
    /// [`INHERIT_RUBRIC_MODEL`], the rubric inherits the parent agent's
    /// model.
    pub rubric_model_spec: Option<String>,
    /// Whether to inherit the parent agent's model. Mirrors the sentinel
    /// state; when true, [`RubricConfig::resolve_model`] defers to the parent
    /// model.
    pub inherit_model: bool,
}

impl Default for RubricConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl RubricConfig {
    /// Construct a default config: `max_iterations` = [`MAX_ITERATIONS`],
    /// `rubric_model_spec` = `None`, `inherit_model` = `true`.
    pub fn new() -> Self {
        Self {
            max_iterations: MAX_ITERATIONS,
            rubric_model_spec: None,
            inherit_model: true,
        }
    }

    /// Resolve the effective model spec for the rubric, given the parent
    /// agent's model.
    ///
    /// Implements the 3-state `_rubric_model_spec` logic:
    /// - If `rubric_model_spec` is `None` and `inherit_model` is true,
    ///   return the parent model (inherit).
    /// - If `rubric_model_spec` is `Some(INHERIT_RUBRIC_MODEL)`, return the
    ///   parent model (explicit inherit).
    /// - Otherwise, return `rubric_model_spec` as-is (dedicated model).
    pub fn resolve_model(&self, parent_model: Option<&str>) -> Option<String> {
        match &self.rubric_model_spec {
            None => {
                if self.inherit_model {
                    parent_model.map(str::to_owned)
                } else {
                    None
                }
            }
            Some(spec) if spec == INHERIT_RUBRIC_MODEL => parent_model.map(str::to_owned),
            Some(spec) => Some(spec.clone()),
        }
    }
}

// ── 9. SelfGradingLoop ────────────────────────────────────────────────

/// Stateful driver for the goal/rubric self-grading loop.
///
/// Owns a [`RubricConfig`] and the current [`GoalState`]. The higher layer
/// drives the loop: invoke the agent, invoke the grader, then call
/// [`SelfGradingLoop::process_grader_response`] with the [`GraderResponse`];
/// inspect the returned [`RubricResult`] and either terminate or inject
/// [`SelfGradingLoop::feedback_for_agent`] and continue.
pub struct SelfGradingLoop {
    /// The rubric configuration.
    pub config: RubricConfig,
    /// The current goal state.
    pub state: GoalState,
}

impl SelfGradingLoop {
    /// Construct a new self-grading loop from a config and an initial state.
    pub fn new(config: RubricConfig, state: GoalState) -> Self {
        Self { config, state }
    }

    /// Apply a grader response to the goal state and return the resulting
    /// [`RubricResult`].
    ///
    /// - On [`GraderVerdict::Satisfied`], the goal is marked
    ///   [`GoalStatus::Complete`], the iteration is advanced, and
    ///   [`RubricResult::Satisfied`] is returned.
    /// - On [`GraderVerdict::NeedsRevision`], the explanation is pushed as
    ///   feedback, the iteration is advanced, and either
    ///   [`RubricResult::NeedsRevision`] or
    ///   [`RubricResult::MaxIterationsReached`] is returned depending on
    ///   whether [`RubricConfig::max_iterations`] has been reached.
    /// - On [`GraderVerdict::Failed`], the goal is marked
    ///   [`GoalStatus::Complete`], the iteration is advanced, and
    ///   [`RubricResult::Failed`] is returned.
    ///
    /// Note: grader errors are produced by the caller and surfaced via
    /// [`RubricResult::GraderError`]; this method does not generate them.
    pub fn process_grader_response(&mut self, response: GraderResponse) -> RubricResult {
        match response.result {
            GraderVerdict::Satisfied => {
                self.state.advance_iteration();
                self.state.status = GoalStatus::Complete;
                RubricResult::Satisfied
            }
            GraderVerdict::NeedsRevision => {
                // Inject the explanation as revision feedback. The char
                // limit is enforced by add_feedback; if the grader produced
                // an oversized explanation, surface a grader error rather
                // than truncating silently.
                if let Err(e) = self.state.add_feedback(response.explanation.clone()) {
                    return RubricResult::GraderError(e.to_string());
                }
                self.state.advance_iteration();
                if self.state.iterations >= self.config.max_iterations {
                    self.state.status = GoalStatus::Complete;
                    RubricResult::MaxIterationsReached
                } else {
                    RubricResult::NeedsRevision
                }
            }
            GraderVerdict::Failed => {
                self.state.advance_iteration();
                self.state.status = GoalStatus::Complete;
                RubricResult::Failed
            }
        }
    }

    /// Return whether the loop should continue: the goal is still active and
    /// the iteration budget has not been exhausted.
    pub fn should_continue(&self) -> bool {
        self.state.status == GoalStatus::Active
            && self.state.iterations < self.config.max_iterations
    }

    /// Return the latest feedback note, if any, for injection into the
    /// agent's next turn.
    pub fn feedback_for_agent(&self) -> Option<String> {
        self.state.feedback.last().cloned()
    }
}

// ── 10. BudgetTracker ─────────────────────────────────────────────────

/// Per-operation budget tracker (simple v0 implementation).
///
/// Tracks four isolated budgets keyed by `operation_id`: context tokens,
/// tool calls, repository tool calls, and web searches. Each budget is
/// checked against its configured maximum on increment; exceeding a budget
/// returns `false` and does not increment the counter.
#[derive(Debug, Clone)]
pub struct BudgetTracker {
    /// Operation id this tracker is scoped to.
    pub operation_id: String,
    /// Current context-token spend.
    pub context_budget: u32,
    /// Current tool-call spend.
    pub tool_call_budget: u32,
    /// Current repository-tool-call spend.
    pub repo_tool_budget: u32,
    /// Current web-search spend.
    pub web_search_budget: u32,
}

impl BudgetTracker {
    /// Construct a new zeroed tracker for the given operation id.
    pub fn new(operation_id: impl Into<String>) -> Self {
        Self {
            operation_id: operation_id.into(),
            context_budget: 0,
            tool_call_budget: 0,
            repo_tool_budget: 0,
            web_search_budget: 0,
        }
    }

    /// Check whether `n` context tokens can be spent without exceeding
    /// `max_context_tokens`.
    pub fn can_spend_context(&self, max_context_tokens: u32, n: u32) -> bool {
        self.context_budget.saturating_add(n) <= max_context_tokens
    }

    /// Attempt to spend `n` context tokens. Returns `true` on success or
    /// `false` (without mutating) if the budget would be exceeded.
    pub fn spend_context(&mut self, max_context_tokens: u32, n: u32) -> bool {
        if !self.can_spend_context(max_context_tokens, n) {
            return false;
        }
        self.context_budget += n;
        true
    }

    /// Check whether a tool call can be spent.
    pub fn can_spend_tool_call(&self, max_tool_calls: u32) -> bool {
        self.tool_call_budget.saturating_add(1) <= max_tool_calls
    }

    /// Attempt to spend one tool call.
    pub fn spend_tool_call(&mut self, max_tool_calls: u32) -> bool {
        if !self.can_spend_tool_call(max_tool_calls) {
            return false;
        }
        self.tool_call_budget += 1;
        true
    }

    /// Check whether a repository tool call can be spent.
    pub fn can_spend_repo_tool(&self, max_repo_tool_calls: u32) -> bool {
        self.repo_tool_budget.saturating_add(1) <= max_repo_tool_calls
    }

    /// Attempt to spend one repository tool call.
    pub fn spend_repo_tool(&mut self, max_repo_tool_calls: u32) -> bool {
        if !self.can_spend_repo_tool(max_repo_tool_calls) {
            return false;
        }
        self.repo_tool_budget += 1;
        true
    }

    /// Check whether a web search can be spent.
    pub fn can_spend_web_search(&self, max_web_searches: u32) -> bool {
        self.web_search_budget.saturating_add(1) <= max_web_searches
    }

    /// Attempt to spend one web search.
    pub fn spend_web_search(&mut self, max_web_searches: u32) -> bool {
        if !self.can_spend_web_search(max_web_searches) {
            return false;
        }
        self.web_search_budget += 1;
        true
    }

    /// Reset all budgets to zero (e.g. for a retry of the same operation).
    pub fn reset(&mut self) {
        self.context_budget = 0;
        self.tool_call_budget = 0;
        self.repo_tool_budget = 0;
        self.web_search_budget = 0;
    }
}

// ── 11. BudgetConfig ──────────────────────────────────────────────────

/// Configuration of the four isolated budgets enforced by
/// [`BudgetTracker`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum context tokens per operation.
    pub max_context_tokens: u32,
    /// Maximum tool calls per operation.
    pub max_tool_calls: u32,
    /// Maximum repository tool calls per operation.
    pub max_repo_tool_calls: u32,
    /// Maximum web searches per operation.
    pub max_web_searches: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 200_000,
            max_tool_calls: 100,
            max_repo_tool_calls: 40,
            max_web_searches: 20,
        }
    }
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;

    /// `GoalStatus` should round-trip through serde in snake_case form.
    fn goal_status_serde() {
        for (status, expected) in [
            (GoalStatus::Active, "\"active\""),
            (GoalStatus::Paused, "\"paused\""),
            (GoalStatus::Blocked, "\"blocked\""),
            (GoalStatus::Complete, "\"complete\""),
        ] {
            let s = serde_json::to_string(&status).expect("serialize");
            assert_eq!(s, expected);
            let back: GoalStatus = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, status);
        }
    }

    /// `GraderResponse` (and nested `GraderVerdict`/`CriterionEval`) should
    /// round-trip through serde.
    fn grader_response_serde() {
        let resp = GraderResponse {
            result: GraderVerdict::NeedsRevision,
            explanation: "criterion X not met".to_string(),
            criteria: vec![
                CriterionEval::Pass { name: "compiles".to_string() },
                CriterionEval::Fail {
                    name: "tests pass".to_string(),
                    gap: "2 tests failing".to_string(),
                },
            ],
        };
        let s = serde_json::to_string(&resp).expect("serialize");
        let back: GraderResponse = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, resp);
    }

    /// `GoalSpec` validation should reject an objective whose combined
    /// length exceeds [`GOAL_APPLICATION_CHAR_LIMIT`].
    fn goal_spec_validation() {
        let too_long = "x".repeat(GOAL_APPLICATION_CHAR_LIMIT + 1);
        let result = GoalSpec::new(too_long, Vec::new());
        let err = result.expect_err("should reject oversized objective");
        match err {
            Error::Goal(deepagents_errors::GoalError::StateSize(_)) => {}
            other => panic!("expected GoalError::StateSize, got {other:?}"),
        }

        // A spec right at the limit should be accepted.
        let at_limit = "x".repeat(GOAL_APPLICATION_CHAR_LIMIT);
        let spec = GoalSpec::new(at_limit, Vec::new()).expect("at-limit accepted");
        assert_eq!(spec.application_char_length(), GOAL_APPLICATION_CHAR_LIMIT);
    }

    /// `GoalState::add_feedback` should enforce
    /// [`GOAL_STATUS_NOTE_CHAR_LIMIT`].
    fn goal_state_add_feedback() {
        let spec = GoalSpec::new("objective".to_string(), vec!["c1".to_string()])
            .expect("valid spec");
        let mut state = GoalState::new(spec);

        // Small feedback accepted.
        state
            .add_feedback("please address X".to_string())
            .expect("small feedback ok");
        assert_eq!(state.feedback.len(), 1);

        // Oversized feedback rejected.
        let too_long = "y".repeat(GOAL_STATUS_NOTE_CHAR_LIMIT + 1);
        let err = state
            .add_feedback(too_long)
            .expect_err("should reject oversized feedback");
        match err {
            Error::Goal(deepagents_errors::GoalError::StateSize(_)) => {}
            other => panic!("expected GoalError::StateSize, got {other:?}"),
        }
        // The rejected feedback should not have been appended.
        assert_eq!(state.feedback.len(), 1);
    }

    /// `RubricConfig::resolve_model` should implement the 3-state
    /// `_rubric_model_spec` semantics.
    fn rubric_config_resolve_model() {
        let parent = Some("claude-sonnet-4");

        // 1. None + inherit_model=true -> inherit parent.
        let cfg = RubricConfig::new();
        assert_eq!(cfg.resolve_model(parent), parent.map(str::to_owned));

        // 2. Some("INHERIT") -> inherit parent.
        let cfg = RubricConfig {
            rubric_model_spec: Some(INHERIT_RUBRIC_MODEL.to_string()),
            ..RubricConfig::new()
        };
        assert_eq!(cfg.resolve_model(parent), parent.map(str::to_owned));

        // 3. Some("<spec>") -> dedicated model.
        let cfg = RubricConfig {
            rubric_model_spec: Some("gpt-5".to_string()),
            inherit_model: false,
            ..RubricConfig::new()
        };
        assert_eq!(cfg.resolve_model(parent), Some("gpt-5".to_string()));

        // 4. None + inherit_model=false -> None.
        let cfg = RubricConfig {
            rubric_model_spec: None,
            inherit_model: false,
            ..RubricConfig::new()
        };
        assert_eq!(cfg.resolve_model(parent), None);
    }

    /// A `Satisfied` grader response should mark the goal complete and
    /// return [`RubricResult::Satisfied`].
    fn self_grading_loop_satisfied() {
        let spec = GoalSpec::new("objective".to_string(), vec!["c1".to_string()])
            .expect("valid spec");
        let state = GoalState::new(spec);
        let mut loop_ = SelfGradingLoop::new(RubricConfig::new(), state);

        let resp = GraderResponse {
            result: GraderVerdict::Satisfied,
            explanation: "all criteria met".to_string(),
            criteria: vec![CriterionEval::Pass { name: "c1".to_string() }],
        };
        let result = loop_.process_grader_response(resp);
        assert_eq!(result, RubricResult::Satisfied);
        assert_eq!(loop_.state.status, GoalStatus::Complete);
        assert_eq!(loop_.state.iterations, 1);
        assert!(!loop_.should_continue());
    }

    /// The loop should return [`RubricResult::MaxIterationsReached`] once
    /// `max_iterations` is exceeded by successive `NeedsRevision` verdicts.
    fn self_grading_loop_max_iterations() {
        let spec = GoalSpec::new("objective".to_string(), vec!["c1".to_string()])
            .expect("valid spec");
        let state = GoalState::new(spec);
        let mut loop_ = SelfGradingLoop::new(RubricConfig::new(), state);

        // max_iterations == 3: two NeedsRevision results stay NeedsRevision,
        // the third should tip over to MaxIterationsReached.
        for i in 0..(MAX_ITERATIONS - 1) {
            let resp = GraderResponse {
                result: GraderVerdict::NeedsRevision,
                explanation: format!("rev {i}"),
                criteria: vec![CriterionEval::Fail {
                    name: "c1".to_string(),
                    gap: "not done".to_string(),
                }],
            };
            let result = loop_.process_grader_response(resp);
            assert_eq!(result, RubricResult::NeedsRevision, "iter {i}");
            assert!(loop_.should_continue(), "should continue after iter {i}");
        }

        // Final NeedsRevision tips over the max.
        let resp = GraderResponse {
            result: GraderVerdict::NeedsRevision,
            explanation: "rev final".to_string(),
            criteria: vec![CriterionEval::Fail {
                name: "c1".to_string(),
                gap: "still not done".to_string(),
            }],
        };
        let result = loop_.process_grader_response(resp);
        assert_eq!(result, RubricResult::MaxIterationsReached);
        assert_eq!(loop_.state.status, GoalStatus::Complete);
        assert!(!loop_.should_continue());

        // feedback_for_agent should return the latest injected note.
        assert_eq!(loop_.feedback_for_agent(), Some("rev final".to_string()));
    }

    // Aggregate entry point so the test names above satisfy the spec's
    // naming convention while remaining callable by `cargo test`.
    #[test]
    fn test_goal_status_serde() {
        goal_status_serde();
    }

    #[test]
    fn test_grader_response_serde() {
        grader_response_serde();
    }

    #[test]
    fn test_goal_spec_validation() {
        goal_spec_validation();
    }

    #[test]
    fn test_goal_state_add_feedback() {
        goal_state_add_feedback();
    }

    #[test]
    fn test_rubric_config_resolve_model() {
        rubric_config_resolve_model();
    }

    #[test]
    fn test_self_grading_loop_satisfied() {
        self_grading_loop_satisfied();
    }

    #[test]
    fn test_self_grading_loop_max_iterations() {
        self_grading_loop_max_iterations();
    }

    #[test]
    fn test_budget_tracker() {
        let cfg = BudgetConfig::default();
        let mut tracker = BudgetTracker::new("op-1");
        assert!(tracker.spend_context(cfg.max_context_tokens, 100));
        assert!(tracker.spend_tool_call(cfg.max_tool_calls));
        assert!(tracker.spend_repo_tool(cfg.max_repo_tool_calls));
        assert!(tracker.spend_web_search(cfg.max_web_searches));
        // over-spend rejected
        let mut t2 = BudgetTracker::new("op-2");
        t2.context_budget = cfg.max_context_tokens;
        assert!(!t2.spend_context(cfg.max_context_tokens, 1));
    }

    #[test]
    fn test_grader_response_jsonschema() {
        // Ensure the JsonSchema derive produces a non-empty schema.
        use schemars::schema_for;
        let schema = schema_for!(GraderResponse);
        let json = serde_json::to_value(&schema).expect("schema to json");
        assert!(json.is_object());
    }
}
