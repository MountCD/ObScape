use crate::{config::AgentConfig, tools::tool::Tool};
use std::collections::HashMap;

pub struct ToolRegistry {
    /// Реестр ещё не подключён к обработчикам — поле появится в работе.
    #[allow(dead_code)]
    tools: HashMap<String, Box<dyn Tool>>,
}
impl ToolRegistry {
    pub fn search_by_server(&self, _name: String) -> Vec<Box<dyn Tool>> {
        todo!() // поиск по первой половине навзания
    }
    pub fn show_avaiable(&self) {
        todo!() // Выдать на эндпоинт все доступные инструменты
    }
    pub fn check_acess(&self, _target: AgentConfig) {
        todo!() // Проверить доступность и режим инструмента  для агента.
    }
}
