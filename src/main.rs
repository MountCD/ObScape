use reqwest::Client;
use tokio;

pub mod llm;
pub mod config;
pub mod database;

async fn test() {
    let client = Client::new();
    // let config = config::load_config();
    let model = llm::Models::TalkModel;
    let message = String::from("Hello, how are you?");

    let answer = llm::make_request(&client, model, message.as_str()).await.unwrap();
    println!("{}", answer);
}

#[tokio::main]
async fn main() {
    test().await;
}