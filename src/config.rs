use std::fs::{read_to_string};
use crate::llm;
use serde::{Serialize, Deserialize};

#[derive(Debug, Deserialize)]
struct Prompt {
    system_prompt: String,
    personal_prompt: String,
}

#[derive(Debug, Deserialize)]
struct TalkModel {
    enabled: bool,
    model_id: String,
    api_url: String,
}
#[derive(Debug, Deserialize)]
struct WorkerModel {
    enabled: bool,
    model_id: String,
    api_url: String,
}

#[derive(Debug, Deserialize)]
struct AudioModel {
    enabled: bool,
    model_id: String,
    api_url: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    vault_path: String,
    prompt: Prompt,
    talk: TalkModel,
    worker: WorkerModel,
    audio: AudioModel,
}