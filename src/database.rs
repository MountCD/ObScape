// Actually, it is not a real sql data base, but a json file
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct Database {
    messages: Vec<JsonMessageContent>,
}
impl Database {
    pub fn open_db() -> Self {
        todo!()
    }

    pub fn write_db(&mut self) {
        todo!()
    }

    pub fn export_messages(self) -> Vec<JsonMessageContent> {
        return self.messages.clone();
    }

    pub fn add_message(&mut self, message: JsonMessageContent) {
        self.messages.push(message);
    }
}

#[warn(dead_code)]
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Roles {
    System,
    User,
    Assistant,
}
#[derive(Debug, Serialize)]
pub struct JsonRequestMessage {
    model: String,
    messages: Vec<JsonMessageContent>, // String -> struct
    stream: bool,
}
impl JsonRequestMessage {
    pub fn new(model: String, messages: Vec<JsonMessageContent>, stream: bool) -> Self {
        JsonRequestMessage {
            model,
            messages,
            stream,
        }
    }
}
#[derive(Debug, Serialize, Clone)]
pub struct ContentStruc {
    time: String,
    chat_id: usize,
    user_id: usize,
    message: String,
}
impl ContentStruc {
    pub fn new(time: String, chat_id: usize, user_id: usize, message: String) -> Self {
        ContentStruc {
            time: time,
            chat_id: chat_id,
            user_id: user_id,
            message: message,
        }
    }
}
#[derive(Debug, Serialize, Clone)]
pub struct JsonMessageContent {
    role: Roles,
    content: ContentStruc,
}
impl JsonMessageContent {
    pub fn new(role: Roles, content: ContentStruc) -> Self {
        JsonMessageContent { role, content }
    }
}
