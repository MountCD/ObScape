use async_trait::async_trait;
use serde_json::Value;
pub enum ToolErrors {
    Denied,
    NotFound,
    Internal(String),
}

impl std::fmt::Display for ToolErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolErrors::Denied => write!(f, "Denied"),
            ToolErrors::NotFound => write!(f, "NotFound"),
            ToolErrors::Internal(e) => write!(f, "Internal: {e}"),
        }
    }
}
#[async_trait]
pub trait Tool {
    fn name(&self) -> String;

    async fn execute(&self, args: Value) -> ToolResponse;
}

pub struct ToolResponse {
    pub succes: bool,
    pub content: Value,
    pub error: Option<ToolErrors>,
}
