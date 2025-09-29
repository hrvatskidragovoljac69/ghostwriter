pub mod anthropic;
pub mod google;
pub mod openai;

use crate::cancellation::GhostwriterCancellation;
use anyhow::Result;
use serde_json::Value as json;
use std::collections::HashMap;

#[async_trait::async_trait]
pub trait LLMEngine {
    fn new(options: &HashMap<String, String>) -> Self
    where
        Self: Sized;
    fn register_tool(&mut self, name: &str, definition: json, callback: Box<dyn FnMut(json) + Send>);
    fn add_text_content(&mut self, text: &str);
    fn add_image_content(&mut self, base64_image: &str);
    fn clear_content(&mut self);
    async fn execute(&mut self, cancellation: &GhostwriterCancellation) -> Result<()>;
}
