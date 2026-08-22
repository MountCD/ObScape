use crate::config;
use crate::database::*;
use anyhow::Result;
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};
//use std::time::SystemTime;

#[derive(Serialize, Debug)]
pub enum Models {
    TalkModel,
    WorkerModel,
    AudioModel,
}

pub async fn make_request(client: &Client, model: Models, message: String) -> Result<String> {
    let config = config::load_config();

    let map_model; // обработка имени модели из конфига
    let api_url_model;
    let api_key_model;

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

    let messages_list = Database::open_db();
    let messages_list = messages_list.export_messages();

    let json_message = JsonRequestMessage::new(map_model.clone(), messages_list.clone(), false);
    dbg!(json!(&json_message));

    let req = json!(json_message);

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
