use super::LLMEngine;
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::Result;
use log::{debug, info};
use serde_json::json;
use serde_json::Value as json;

pub struct Tool {
    name: String,
    definition: json,
    callback: Option<Box<dyn FnMut(json) + Send>>,
}

pub struct OpenAI {
    model: String,
    base_url: String,
    api_key: String,
    tools: Vec<Tool>,
    content: Vec<json>,
}

impl OpenAI {
    fn openai_tool_definition(tool: &Tool) -> json {
        json!({
                "type": "function",
                "function": {
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "parameters": tool.definition["parameters"],
                }
        })
    }

    pub fn add_content(&mut self, content: json) {
        self.content.push(content);
    }
}

#[async_trait::async_trait]
impl LLMEngine for OpenAI {
    fn new(options: &OptionMap) -> Self {
        let api_key = option_or_env(options, "api_key", "OPENAI_API_KEY");
        let base_url = option_or_env_fallback(options, "base_url", "OPENAI_BASE_URL", "https://api.openai.com");
        let model = options.get("model").unwrap().to_string();

        Self {
            model,
            base_url,
            api_key,
            tools: Vec::new(),
            content: Vec::new(),
        }
    }

    fn register_tool(&mut self, name: &str, definition: json, callback: Box<dyn FnMut(json) + Send>) {
        self.tools.push(Tool {
            name: name.to_string(),
            definition,
            callback: Some(callback),
        });
    }

    fn add_text_content(&mut self, text: &str) {
        self.add_content(json!({
            "type": "text",
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.add_content(json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:image/png;base64,{}", base64_image)
            }
        }));
    }

    fn clear_content(&mut self) {
        self.content.clear();
    }

    async fn execute(&mut self, _cancellation: &crate::cancellation::GhostwriterCancellation, _status_callback: Option<super::StatusCallback>) -> Result<()> {
        let body = json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": self.content
            }],
            "tools": self.tools.iter().map(Self::openai_tool_definition).collect::<Vec<_>>(),
            "tool_choice": "required",
            "parallel_tool_calls": false
        });

        // print body for debugging
        debug!("Request: {}", body);
        let raw_response = ureq::post(format!("{}/v1/chat/completions", self.base_url).as_str())
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(&body);

        let mut response = match raw_response {
            Ok(response) => response,
            Err(err) => {
                info!("API Error: {}", err);
                return Err(anyhow::anyhow!("API ERROR: {}", err));
            }
        };

        // Read response body as string
        let body_text = response.body_mut().read_to_string().unwrap();
        let json: json = serde_json::from_str(&body_text).unwrap();
        debug!("Response: {}", json);

        let tool_calls = &json["choices"][0]["message"]["tool_calls"];

        if let Some(tool_call) = tool_calls.get(0) {
            let function_name = tool_call["function"]["name"].as_str().unwrap();
            let function_input_raw = tool_call["function"]["arguments"].as_str().unwrap();
            let function_input = serde_json::from_str::<json>(function_input_raw).unwrap();
            let tool = self.tools.iter_mut().find(|tool| tool.name == function_name);

            if let Some(tool) = tool {
                if let Some(callback) = &mut tool.callback {
                    callback(function_input.clone());
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("No callback registered for tool {}", function_name))
                }
            } else {
                Err(anyhow::anyhow!("No tool registered with name {}", function_name))
            }
        } else {
            Err(anyhow::anyhow!("No tool calls found in response"))
        }
    }
}
