use ollama_rs::error::OllamaError;
use ollama_rs::Ollama;
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::models::create::{CreateModelRequest, CreateModelStatus};
//use tokio_stream::StreamExt;

// Get ollama models from default port:
fn get_ollama() -> Ollama {
    Ollama::default()
}
pub async fn test() -> Result<(), Box<dyn std::error::Error>> {
    let model = "gemma4:cloud".to_string();
    let ollama = get_ollama();
    let prompt = "Hello, how are you?".to_string();

    let response = ollama.generate(GenerationRequest::new(model, prompt)).await;
    response.unwrap_err();

    Ok(())
}
// Here is our personality is being used:
async fn make_model(config: String) -> Result<CreateModelStatus, OllamaError> {
    let ollama = get_ollama();
    let binding = ollama.clone();
    binding.create_model(CreateModelRequest::new("obsistent".into())
        .system(config.into())
        .from_model("gemma4".into())).await
}

