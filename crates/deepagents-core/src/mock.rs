//! Mock completion model for testing (Q8).
//!
//! [`MockCompletionModel`] implements rig's `CompletionModel` trait by
//! dequeuing pre-programmed responses in FIFO order. This lets tests drive
//! the full agent run-loop without a real LLM provider.
//!
//! # Example
//!
//! ```no_run
//! use deepagents_core::mock::MockCompletionModel;
//! use rig_core::completion::{CompletionModel, CompletionRequest};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let model = MockCompletionModel::from_responses(vec![
//!     "Hello, world!".to_string(),
//! ]);
//! # Ok(())
//! # }
//! ```
//!
//! See `docs/SPEC.md` §Q8 for the testing strategy.

use std::sync::Arc;
use std::sync::Mutex;

use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
    Usage,
};
use rig_core::message::{AssistantContent, Text};
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;

/// A mock completion model that returns pre-programmed responses in order.
///
/// Each call to `completion()` dequeues the next queued response. If the
/// queue is empty, it returns an error. This is the primary testing primitive
/// for the SDK: it lets unit tests drive the full `AgentRun` state machine
/// without any network access.
#[derive(Debug, Clone)]
pub struct MockCompletionModel {
    responses: Arc<Mutex<Vec<Vec<AssistantContent>>>>,
    provider_name: String,
}

impl MockCompletionModel {
    /// Create a mock model with a single empty placeholder response.
    ///
    /// Convenience for builder/config tests that never actually call the model.
    pub fn new() -> Self {
        Self::single("")
    }

    /// Create a mock model from a list of text responses.
    ///
    /// Each response is a single text message. For multi-content responses
    /// (e.g. with tool calls), use [`from_content`](Self::from_content).
    pub fn from_responses(responses: Vec<String>) -> Self {
        let content: Vec<Vec<AssistantContent>> = responses
            .into_iter()
            .map(|text| vec![AssistantContent::Text(Text::new(text))])
            .collect();
        Self::from_content(content)
    }

    /// Create a mock model from a list of pre-built assistant content blocks.
    ///
    /// Each inner `Vec<AssistantContent>` is returned as-is for one completion call.
    pub fn from_content(responses: Vec<Vec<AssistantContent>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            provider_name: "mock".to_string(),
        }
    }

    /// Create a single-response mock (convenience for one-shot tests).
    pub fn single(text: impl Into<String>) -> Self {
        Self::from_responses(vec![text.into()])
    }

    /// Create an empty mock (all calls will error).
    pub fn empty() -> Self {
        Self::from_content(Vec::new())
    }

    /// Set the provider name reported in `CompletionResponse`.
    pub fn with_provider(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    /// Number of remaining queued responses.
    pub fn remaining(&self) -> usize {
        self.responses.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Dequeue the next response, or return an error if empty.
    fn dequeue(&self) -> Result<Vec<AssistantContent>, CompletionError> {
        let mut guard = self.responses.lock().unwrap_or_else(|e| e.into_inner());
        guard.pop_first().ok_or_else(|| {
            CompletionError::ResponseError("mock response queue is empty".to_string())
        })
    }
}

impl CompletionModel for MockCompletionModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend {
        let provider = self.provider_name.clone();
        async move {
            let choice = self.dequeue()?;
            Ok(CompletionResponse::new(choice, Usage::new(), provider))
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<StreamingCompletionResponse, CompletionError>> + WasmCompatSend {
        async {
            Err(CompletionError::ResponseError(
                "MockCompletionModel does not support streaming".to_string(),
            ))
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// A trait extension for `Vec` to pop from the front without importing `Deque`.
trait PopFirst<T> {
    fn pop_first(&mut self) -> Option<T>;
}

impl<T> PopFirst<T> for Vec<T> {
    fn pop_first(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::CompletionRequestBuilder;
    use rig_core::message::Message;

    fn make_request() -> CompletionRequest {
        let model = MockCompletionModel::empty();
        CompletionRequestBuilder::new(model, Message::user("test")).build()
    }

    #[tokio::test]
    async fn test_single_response() {
        let model = MockCompletionModel::single("Hello!");
        let req = make_request();
        // We need to use the model itself, not the empty one in make_request.
        // The completion method ignores the request, so this works.
        let response = model.completion(req).await.unwrap();
        assert_eq!(response.choice.len(), 1);
        match &response.choice[0] {
            AssistantContent::Text(text) => assert_eq!(text.text, "Hello!"),
            _ => panic!("expected text content"),
        }
    }

    #[tokio::test]
    async fn test_multiple_responses_in_order() {
        let model = MockCompletionModel::from_responses(vec![
            "first".to_string(),
            "second".to_string(),
        ]);
        assert_eq!(model.remaining(), 2);

        let r1 = model.completion(make_request()).await.unwrap();
        let r2 = model.completion(make_request()).await.unwrap();

        match &r1.choice[0] {
            AssistantContent::Text(t) => assert_eq!(t.text, "first"),
            _ => panic!(),
        }
        match &r2.choice[0] {
            AssistantContent::Text(t) => assert_eq!(t.text, "second"),
            _ => panic!(),
        }
        assert_eq!(model.remaining(), 0);
    }

    #[tokio::test]
    async fn test_empty_queue_errors() {
        let model = MockCompletionModel::empty();
        let result = model.completion(make_request()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_provider_name() {
        let model = MockCompletionModel::single("hi").with_provider("test-provider");
        let response = model.completion(make_request()).await.unwrap();
        assert_eq!(response.provider, "test-provider");
    }
}
