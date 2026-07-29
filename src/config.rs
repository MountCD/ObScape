use std::error::Error;
use std::fs::{read_to_string};
use walkdir::{DirEntry, WalkDir};
use crate::llm;

struct Config {
    system: String,
    personality: String,
}
impl Config {
    pub fn new() -> Config {
        Config {
            system: "system".to_string(),
            personality: "personality".to_string(),
        }
    }

    fn push_system(&mut self, name: &str) {
        self.system.push_str(name);
    }
    fn push_personality(&mut self, name: &str) {
        self.personality.push_str(name);
    }

    fn export(&self) -> String {
        let mut output: String = String::new();
        output.push_str(&self.system);
        output.push_str(&self.personality);
        output
    }
}

fn read_md(file: walkdir::Result<DirEntry>) -> Result<String, Box<dyn Error>> {
    let file = file?;
    let file = file.file_name();
    let buffer = read_to_string(file)?;

    Ok(buffer)
}

pub fn open_config() -> String {
    let binding = std::env::current_dir().unwrap();
    let _config_path: &str = binding.to_str().unwrap();
    let mut config = Config::new();

    for entry in WalkDir::new(&_config_path) {
        if !entry.as_ref().unwrap().path().is_dir() {
            continue;
        }
        if entry.as_ref().unwrap().path().ends_with("system.md") {
            config.push_system(&*read_md(entry).unwrap());
        } else if entry.as_ref().unwrap().path().ends_with("personality.md") {
            config.push_personality(&*read_md(entry).unwrap());
        }
    }

    config.export()
}

