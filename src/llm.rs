use std::collections::HashMap;
use anyhow::Result;
use crate::config;
use reqwest;
use serde_json::{json, to_value, Value};

enum Models {
    TalkModel,
    WorkerModel,
    AudioModel
}
fn make_map(model: Models) -> Result<HashMap<String, Value>> {
    let config = config::load_config();

    let map_model = String::new(); // обработка имени модели из конфига
    match model {
        Models::TalkModel => { let map_model = config.talk.model_id; }
        Models::WorkerModel => { let map_model = config.worker.model_id; }
        Models::AudioModel => { let map_model = config.audio.model_id; }
    }

    let req = json!({
        "model": map_model,
        "messages": [
            {
                "role": "system",
                "message": config.prompt.merge()
            }
            
        ]
    });

    todo!("make_map");
}