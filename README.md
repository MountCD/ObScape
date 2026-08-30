# ObScape
> A core to integrate AI into your project easily.
## Installation:
WIP.
## Usage:
### Via HTTP:
> API Endpoints is defined in AGENTS.md, soon i will wrote them here.
### Via rust library:
Use Assistant structure:
 - new(db: Database, cfg: Config)' - for new Assistant.
 - 'send_message(&self, user_id: i64, chat_id: i64, message: String)' - to send a message.
 - 'create_chat(&self, user_id: i64, message: String)' - to make a new chat.
## Plans:
- [x] Make automatic config generation.
- [x] Make verbose mode.
- [ ] Make library for other languages
