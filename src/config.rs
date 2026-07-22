use walkdir::WalkDir;
struct Config {
    system: String,
    personality: String
}
impl Config {
    pub fn new() -> Config {
        Config { system: "system".to_string(), personality: "personality".to_string() }
    }

    fn push_system(&mut self, name: &str) {
        self.system.push_str(name);
    }
    fn push_personality(&mut self, name: &str) {
        self.personality.push_str(name);
    }

    fn export(&self) -> String {
        let mut output: String  = String::new();
        output.push_str(&self.system);
        output.push_str(&self.personality);
        output
    }
}

pub fn open_config() -> String {
    let binding = std::env::current_dir().unwrap();
    let _config_path: &str = binding.to_str().unwrap();
    let mut config = Config::new();

    for entry in WalkDir::new(&_config_path) {
        if !entry.as_ref().unwrap().path().is_dir() {
            continue;
        }
        if entry.as_ref().unwrap().path().ends_with("system.conf") {
            config.push_system(entry.unwrap().path().to_str().unwrap())
        } else if entry.as_ref().unwrap().path().ends_with("personality.conf") {
            config.push_personality(entry.unwrap().path().to_str().unwrap())
        }
    }

    config.export()
}