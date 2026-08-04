use crate::config;
use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Debug)]
pub enum Models {
    TalkModel,
    WorkerModel,
    AudioModel,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Roles {
    System,
    User,
    Assistant,
}
#[derive(Debug, Serialize)]
struct JsonRequest {
    model: Models,
    messages: Vec<JsonMessageContent>, // String -> struct
    stream: bool,
}
impl JsonRequest {
    pub fn new(model: Models, messages: Vec<JsonMessageContent>, stream: bool) -> Self {
        JsonRequest { model, messages, stream }
    }
}
#[derive(Debug, Serialize)]
struct JsonMessageContent {
    role: Roles,
    message: String,
}
impl JsonMessageContent {
    pub fn new(role: Roles, message: String) -> Self {
        JsonMessageContent { role, message }
    }
}
pub async fn make_request(client: &Client, model: Models, message: &str) -> Result<String> {
    let config = config::load_config();

    let map_model; // обработка имени модели из конфига
    let api_url_model ;
    let api_key_model ;

    match model {
        Models::TalkModel => {
            map_model = config.talk.model_id;
            api_url_model = config.talk.api_url;
            api_key_model = config.talk.api_key;
        }
        Models::WorkerModel => {
            map_model = config.worker.model_id;
            api_url_model = config.worker.api_url;
            api_key_model = config.worker.api_key;
        }
        Models::AudioModel => {
            map_model = config.audio.model_id;
            api_url_model = config.audio.api_url;
            api_key_model = config.audio.api_key;
        }
    }

    let req = json!({
        "model": map_model,
        "messages": [
            {
                "role": "system",
                "content": config.prompt.merge().to_string()
            },
            {
                "role": "user",
                "content": message.to_string()
            }
        ]
    });

    let mut req = client.post(api_url_model).json(&req);
    req = req.bearer_auth(api_key_model);
    dbg!(&req);

    let response = req.send().await?;
    dbg!(&response);
    let response = response.json::<Value>().await?;
    dbg!(&response);

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("Ошибка: пустой ответ")
        .to_string();
    dbg!(&content);

    Ok(content)
}
