use serde_json::Value;
use tokio;
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

pub trait Tool {
    fn name(&self) -> String;
    
    async fn execute(
        &self,
        args: Value,
    ) -> ToolResponse;
}

pub struct ToolResponse {
    pub succes: bool,
    pub content: Value,
    pub error: Option<ToolErrors>
}

#[tokio::test]
async fn test_tool() {
    struct Test {
        name: String
    }
    impl Tool for Test {
        fn name(&self) -> String {
            self.name.clone()
        }
    
        async fn execute(
            &self,
            args: Value,
        ) -> ToolResponse {
            ToolResponse {
                succes: true,
                content: args,
                error: None
            }
        }
    }

    let tes_tool = Test {
        name: String::from("MyTool")
    };
    let response = tes_tool.execute(
        Value::String(tes_tool.name())
    );
    assert_eq!(response.await.succes, true)
}