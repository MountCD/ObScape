use serde::{Deserialize, Serialize};
use rusqlite;
#[derive(Serialize, Deserialize, Debug, Clone)]
struct ChatMessage {
    role: String,    // "system" | "user" | "assistant"
    content: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}