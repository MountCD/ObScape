use std::collections::HashMap;
use crate::{config::AgentConfig, tools::tool::Tool};

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}
impl ToolRegistry {
    pub fn search_by_server(&self, name: String) -> Vec<Box<dyn Tool>>{
        todo!() // поиск по первой половине навзания
    }
    pub fn show_avaiable(&self) {
        todo!() // Выдать на эндпоинт все доступные инструменты
    }
    pub fn check_acess(&self, target: AgentConfig) {
        todo!() // Проверить доступность и режим инструмента  для агента.
    }
}
