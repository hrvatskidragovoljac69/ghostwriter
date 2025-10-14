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

pub struct Google {
    model: String,
    base_url: String,
    api_key: String,
    tools: Vec<Tool>,
    content: Vec<json>,
}

impl Google {
    fn google_tool_definition(tool: &Tool) -> json {
        json!({
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "parameters": tool.definition["parameters"],
        })
    }

    pub fn add_content(&mut self, content: json) {
        self.content.push(content);
    }
}

#[async_trait::async_trait]
impl LLMEngine for Google {
    fn new(options: &OptionMap) -> Self {
        let api_key = option_or_env(options, "api_key", "GOOGLE_API_KEY");
        let base_url = option_or_env_fallback(options, "base_url", "GOOGLE_BASE_URL", "https://generativelanguage.googleapis.com");
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
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.add_content(json!({
            "inline_data": {
                "mime_type": "image/png",
                "data": base64_image,
            }
        }));
    }

    fn clear_content(&mut self) {
        self.content.clear();
    }

    async fn execute(&mut self, _cancellation: &crate::cancellation::GhostwriterCancellation, _status_callback: Option<super::StatusCallback>) -> Result<()> {
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": self.content
            }],
            "tools": [{ "function_declarations": self.tools.iter().map(Self::google_tool_definition).collect::<Vec<_>>() }],
            "tool_config": {
                "function_calling_config": {
                    "mode": "ANY"
                }
            }
        });

        // print body for debugging
        debug!("Request: {}", body);
        let raw_response = ureq::post(format!("{}/v1beta/models/{}:generateContent?key={}", self.base_url, self.model, self.api_key).as_str())
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

        let tool_calls = &json["candidates"][0]["content"]["parts"];

        if let Some(tool_call) = tool_calls.get(0) {
            let function_name = tool_call["functionCall"]["name"].as_str().unwrap();
            let function_input = &tool_call["functionCall"]["args"];
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
