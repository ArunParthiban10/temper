//! LLM provider trait and implementations.
//!
//! The [`LlmProvider`] trait abstracts over different LLM backends.
//! Currently provides [`AnthropicProvider`] for the Anthropic Messages API.

pub mod anthropic;
pub mod codex;
pub(crate) mod sse;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A content block in an LLM message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// A text content block.
    #[serde(rename = "text")]
    Text { text: String },
    /// A tool use request from the LLM.
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// A tool result sent back to the LLM.
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Parsed LLM response.
#[derive(Debug)]
pub struct LlmResponse {
    /// The content blocks in the response.
    pub content: Vec<ContentBlock>,
    /// The reason the LLM stopped generating.
    pub stop_reason: String,
}

/// A message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// The role: "user" or "assistant".
    pub role: String,
    /// The content blocks.
    pub content: Vec<ContentBlock>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_block_text_round_trip() {
        let block = ContentBlock::Text { text: "hello".to_string() };
        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized["type"], "text");
        assert_eq!(serialized["text"], "hello");
        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            other => panic!("Expected Text, got {other:?}"),
        }
    }

    #[test]
    fn content_block_tool_use_round_trip() {
        let block = ContentBlock::ToolUse {
            id: "tu_1".to_string(),
            name: "search".to_string(),
            input: json!({"query": "test"}),
        };
        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized["type"], "tool_use");
        assert_eq!(serialized["name"], "search");
        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "search");
                assert_eq!(input["query"], "test");
            }
            other => panic!("Expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn content_block_tool_result_round_trip() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu_1".to_string(),
            content: "result text".to_string(),
            is_error: Some(true),
        };
        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized["type"], "tool_result");
        assert_eq!(serialized["is_error"], true);
        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                assert_eq!(tool_use_id, "tu_1");
                assert_eq!(content, "result text");
                assert_eq!(is_error, Some(true));
            }
            other => panic!("Expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_omits_is_error_when_none() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu_1".to_string(),
            content: "ok".to_string(),
            is_error: None,
        };
        let serialized = serde_json::to_string(&block).unwrap();
        assert!(!serialized.contains("is_error"));
    }

    #[test]
    fn message_round_trip() {
        let msg = Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text { text: "hi".to_string() }],
        };
        let serialized = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialized["role"], "user");
        let deserialized: Message = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.role, "user");
        assert_eq!(deserialized.content.len(), 1);
    }
}

/// Trait for pluggable LLM providers.
///
/// Implementations handle the details of API calls, authentication, and
/// response parsing for a specific LLM backend.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a non-streaming request to the LLM.
    async fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[serde_json::Value],
    ) -> Result<LlmResponse>;

    /// Send a streaming request to the LLM.
    ///
    /// The `on_delta` callback is invoked with text deltas as they arrive,
    /// enabling real-time output. Takes `String` to avoid lifetime issues
    /// with `Box<dyn Fn>` drop ordering in Rust 2024 edition.
    async fn send_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[serde_json::Value],
        on_delta: Box<dyn Fn(String) + Send>,
    ) -> Result<LlmResponse>;
}
