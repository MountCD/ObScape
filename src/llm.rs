use crate::config;
use anyhow::Result;
use reqwest::{header, Client, Response};
use serde_json::{Value, json, to_value};
use std::collections::HashMap;

pub enum Models {
    TalkModel,
    WorkerModel,
    AudioModel,
}
pub async fn make_request(client: &Client, model: Models, message: &str) -> Result<String> {
    let config = config::load_config();

    let map_model = String::new(); // обработка имени модели из конфига
    let api_url_model = String::new();
    let api_key_model = String::new();
    match model {
        Models::TalkModel => {
            let map_model = config.talk.model_id;
            let api_url_model = config.talk.api_url;
            let api_key_model = config.talk.api_key;
        }
        Models::WorkerModel => {
            let map_model = config.worker.model_id;
            let api_url_model = config.worker.api_url;
            let api_key_model = config.worker.api_key;
        }
        Models::AudioModel => {
            let map_model = config.audio.model_id;
            let api_url_model = config.audio.api_url;
            let api_key_model = config.audio.api_key;
        }
    }

    let req = json!({
        "model": map_model,
        "messages": [
            {
                "role": "system",
                "message": config.prompt.merge()
            },
            {
                "role": "user",
                "message": message
            }
        ]
    });

    let mut req = client.post(api_url_model).json(&req);
    req = req.bearer_auth(api_key_model);

    let response = req.send().await?.json::<serde_json::Value>().await?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("Ошибка: пустой ответ")
        .to_string();

    Ok(content)
}
