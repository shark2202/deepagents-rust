//! DeepAgents 的 state 定义。
//!
//! # Phase 1（MVP）
//!
//! 自建 `#[derive(State)] struct DeepAgentState`，单 `messages` 字段用 `messages_reducer`
//! （matching-ID 更新 / remove 哨兵删除 / 新 ID 追加）——行为与 juncture `MessagesState`
//! 及 deepagents `add_messages` 语义对齐。自建而非 `type` alias 是因为 `juncture-derive`
//! 生成的 `MessagesStateUpdate` 未从 facade 公开导出，外部 crate 无法构造；
//! 且 Phase 2 加中间件私有状态字段本就需要自建。
//!
//! # defer：DeltaChannel
//!
//! deepagents 的 `DeepAgentState` 用 `DeltaChannel(_messages_delta_reducer, snapshot_frequency=50)`
//! 把 checkpoint 增长从 O(N²) 降到 O(N)。juncture 有 `DeltaChannel` 类型，但其 `checkpoint()`
//! 实现是**全量快照**（非真增量），直接用拿不到优化收益。DeltaChannel 是纯性能优化、非行为正确性，
//! 故 defer 到 Phase 3；届时需在 deepagents-rust 内自建真增量 channel。
//!
//! # Phase 2 扩展
//!
//! 中间件的私有状态字段（`_summarization_event`、`memory_contents` 等，deepagents 用 `PrivateStateAttr`）
//! 将用 `UntrackedChannel`（不进 checkpoint）的独立字段承载，并在 subagent 调用前剥离。

use juncture::state::messages::{Message, messages_reducer};
use juncture_derive::State;
use serde::{Deserialize, Serialize};

/// DeepAgents 的 state。行为等价 deepagents `DeepAgentState`（messages + `add_messages` 语义），
/// 仅缺 DeltaChannel 性能优化（defer Phase 3）。
///
/// `#[derive(State)]` 生成 `DeepAgentStateUpdate`（同 crate 可达），用于 `Command::update`。
#[derive(State, Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeepAgentState {
    /// 对话消息，`add_messages` 语义合并。
    #[reducer(custom = messages_reducer)]
    pub messages: Vec<Message>,
}
