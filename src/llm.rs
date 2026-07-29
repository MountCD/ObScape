use async_trait::async_trait;
use anyhow::Result;
#[async_trait]
pub trait LanguageModel: Send + Sync {
    fn name(&self) -> &str;
    async fn completion(&self, prompt: &str, history: &str) -> Result<String>;

}

pub struct LmStudioModel {
    pub name: String,
    pub model_id: String, // например "gemma-4-e2b" или "gemma-4-12b"
    pub api_url: String,  // например "http://localhost:1234/v1"
    pub temperature: f32,
}

#[async_trait]
impl LanguageModel for LmStudioModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn completion(&self, prompt: &str, history: &str) -> Result<String> {
        // Здесь один общий HTTP-запрос через reqwest к LM Studio OpenAI-like API
        // Подставляем self.model_id и self.api_url
        todo!()
    }
}