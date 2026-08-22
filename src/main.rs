use reqwest::Client;
use snailquote::unescape;
use tokio;
pub mod config;
pub mod database;
pub mod llm;

#[tokio::test]
async fn make_test_request() {
    let client = Client::new();
    // let config = config::load_config();
    let model = llm::Models::TalkModel;
    let message = String::from(
        "Твоя задача - сделать запрос другой более сложной модели при помощи инструмента. Например: скажи сложной найти определенный по смыслу контент среди множества файлов. Оформи своё сообщение в json так, как это выглядело бы при передаче другой модели.",
    );

    let answer = llm::make_request(&client, model, message).await;
    println!("{:?}", answer);
}

#[tokio::main]
async fn main() {
    println!("WIP")
}
