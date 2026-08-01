use serde::{Deserialize, Serialize};
//extern crate rusqlite;
//use rusqlite::{Connection, Result, params}; in alpha we will use JSON format for storing messages history.
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

//i will skip it for now, continue later.