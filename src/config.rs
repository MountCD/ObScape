use serde::Deserialize;
use std::fs;

#[derive(Debug, Deserialize)]
pub struct Prompt {
    pub system_prompt: String,
    pub personal_prompt: String,
}
impl Prompt {
    pub fn merge(&self) -> String {
        let mut new_prompt = String::new();
        new_prompt.push_str(&self.system_prompt);
        new_prompt.push_str("");
        new_prompt.push_str(&self.personal_prompt);
        new_prompt
    }
}
#[derive(Debug, Deserialize)]
pub struct TalkModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
}
#[derive(Debug, Deserialize)]
pub struct WorkerModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
}
#[derive(Debug, Deserialize)]
pub struct AudioModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
}
#[derive(Debug, Deserialize)]
pub struct Config {
    pub vault_path: String,
    pub prompt: Prompt,
    pub talk: TalkModel,
    pub worker: WorkerModel,
    pub audio: AudioModel,
}

pub fn load_config() -> Config {
    let filename = "config.toml";
    let contents = fs::read_to_string(filename).expect("Something went wrong reading the file");

    // 2. Parse TOML string into Config struct
    let config: Config = toml::from_str(&contents).expect("Failed to parse TOML config");

    config
}
