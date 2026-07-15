use anyhow::Result;
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio::sync::mpsc;
use url::Url;

use crate::core::config::Config;
use crate::core::models::{
    ChatRequest, ImageUrlContent, Message, MessageContent, MessageContentPart, ResponseChoice,
    ResponseDelta, ResponseEvent, Tool,
};

const MAX_VISIBLE_CITATIONS: usize = 12;
const MAX_UPSTREAM_ERROR_CHARS: usize = 500;
const CHATGPT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_CLI_VERSION: &str = "0.144.0";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.144.0";

fn responses_message_content(message: &Message) -> Value {
    match &message.content {
        None => Value::String(String::new()),
        Some(MessageContent::Text(text)) => Value::String(text.clone()),
        Some(MessageContent::Parts(parts)) => Value::Array(
            parts
                .iter()
                .map(|part| match part {
                    MessageContentPart::Text { text } => json!({
                        "type": "input_text",
                        "text": text,
                    }),
                    MessageContentPart::ImageUrl {
                        image_url: ImageUrlContent { url, detail },
                    } => {
                        let mut translated = json!({
                            "type": "input_image",
                            "image_url": url,
                        });
                        if let Some(detail) = detail {
                            translated["detail"] = json!(detail.as_str());
                        }
                        translated
                    }
                })
                .collect(),
        ),
    }
}

/// Convert accepted Chat Completions history into Responses input items.
/// The HTTP handler validates every content part before this function is used.
fn build_responses_input(messages: &[Message]) -> Vec<Value> {
    let mut input_messages = Vec::new();

    for msg in messages {
        let text_content = msg.text_content();
        match msg.role.as_str() {
            "system" => {
                input_messages.push(json!({
                    "role": "user",
                    "content": format!("<system>\n{}\n</system>", text_content)
                }));
            }
            "tool" => {
                // Responses API: a tool result is a function_call_output item linked by call_id
                // (not free text). Fall back to the old text wrap only if no id is present.
                match &msg.tool_call_id {
                    Some(call_id) => input_messages.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": text_content
                    })),
                    None => input_messages.push(json!({
                        "role": "assistant",
                        "content": format!("<tool_response>\n{}\n</tool_response>", text_content)
                    })),
                }
            }
            "assistant" => {
                // Assistant text (if any) as a message, then each tool call as a function_call
                // item - the backend needs the full function-calling turn to continue a tool loop.
                if !text_content.is_empty() {
                    input_messages.push(json!({ "role": "assistant", "content": text_content }));
                }
                if let Some(calls) = msg.tool_calls.as_ref().and_then(|v| v.as_array()) {
                    for tc in calls {
                        let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let func = tc.get("function");
                        let name = func
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args = func
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        input_messages.push(json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": args
                        }));
                    }
                }
            }
            "user" | "developer" => {
                input_messages.push(json!({
                    "role": msg.role,
                    "content": responses_message_content(msg)
                }));
            }
            _ => {
                input_messages.push(json!({
                    "role": "user",
                    "content": format!("<{}>\n{}\n</{}>", msg.role, text_content, msg.role)
                }));
            }
        }
    }

    input_messages
}

fn map_tools_for_responses(tools: &[Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| match tool {
            Tool::Function { function } => {
                let mut mapped = json!({
                    "type": "function",
                    "name": function.name,
                    "parameters": function.parameters,
                    // Chat Completions treats an omitted `strict` flag as false.
                    // Do not silently strengthen ordinary client schemas: the
                    // Responses API then requires `additionalProperties: false`
                    // on every object, which rejects Ouroboros's valid non-strict
                    // tool envelope before the model can answer.
                    "strict": function.strict.unwrap_or(false),
                });
                if let Some(description) = &function.description {
                    mapped["description"] = json!(description);
                }
                mapped
            }
            Tool::WebSearch {
                external_web_access,
                search_context_size,
                max_uses: _,
            } => {
                let mut mapped = json!({
                    "type": "web_search",
                    "external_web_access": external_web_access.unwrap_or(true),
                });
                if let Some(search_context_size) = search_context_size {
                    mapped["search_context_size"] = json!(search_context_size);
                }
                mapped
            }
        })
        .collect()
}

fn build_responses_payload(
    request: &ChatRequest,
    instructions: String,
    input_messages: Vec<Value>,
) -> Value {
    let mapped_tools = map_tools_for_responses(&request.tools);
    let mut payload = json!({
        "model": request.model,
        "instructions": instructions,
        "input": input_messages,
        "store": false,
        "stream": true,
    });

    if !mapped_tools.is_empty() {
        payload["tools"] = json!(mapped_tools);
        payload["tool_choice"] = json!("auto");
        payload["parallel_tool_calls"] = json!(false);
    }
    payload
}

fn build_codex_request(
    client: &Client,
    access_token: &str,
    account_id: &str,
    session_id: &str,
    payload: &Value,
) -> RequestBuilder {
    client
        .post(CHATGPT_CODEX_RESPONSES_URL)
        .bearer_auth(access_token)
        .header("chatgpt-account-id", account_id)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header("OpenAI-Beta", "responses=experimental")
        .header("session_id", session_id)
        .header("originator", CODEX_ORIGINATOR)
        .header(reqwest::header::USER_AGENT, CODEX_USER_AGENT)
        .json(payload)
}

pub async fn stream_chat_completions(
    config: &Config,
    request: ChatRequest,
) -> Result<mpsc::Receiver<Result<ResponseEvent>>> {
    let client = Client::new();
    let (tx, rx) = mpsc::channel(100);
    let config = config.clone();

    tokio::spawn(async move {
        // SOLUTION: Convert system messages to user messages with special formatting
        // ChatGPT Responses API has strict validation on instructions field
        // So we put system messages in the input array as user messages

        let input_messages = build_responses_input(&request.messages);
        let image_part_count = request.image_part_count();

        // Use the full base instructions from prompt.md
        use crate::core::client_common::BASE_INSTRUCTIONS;

        let mut instructions = BASE_INSTRUCTIONS.to_string();

        // Add user instructions from AGENTS.md if available
        if let Some(user_instructions) = &config.user_instructions {
            instructions.push_str("\n\n<user_instructions>\n\n");
            instructions.push_str(user_instructions);
            instructions.push_str("\n\n</user_instructions>");
        }

        println!("🔍 DEBUG - Processing {} messages", request.messages.len());
        println!(
            "🔍 DEBUG - Instructions length: {} characters",
            instructions.len()
        );
        println!("🔍 DEBUG - Input messages: {}", input_messages.len());
        println!("🔍 DEBUG - Image parts: {}", image_part_count);

        println!("🔍 DEBUG - Tools in request: {}", request.tools.len());
        let payload = build_responses_payload(&request, instructions, input_messages);
        let mapped_tool_count = payload
            .get("tools")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        println!("🔍 DEBUG - Mapped tools included: {}", mapped_tool_count);
        println!("🔍 DEBUG - Has valid tools: {}", mapped_tool_count > 0);
        // Never log the full payload: multimodal requests may contain private
        // image URLs or megabytes of base64-encoded image data.
        println!("🔍 DEBUG - Request payload prepared (content redacted)");

        // Get access token and account ID
        println!("🔑 Getting access token...");
        let access_token = match get_access_token(&config).await {
            Ok(token) => {
                println!("Access token retrieved");
                token
            }
            Err(e) => {
                println!("❌ Access token retrieval failed: {}", e);
                let _ = tx
                    .send(Err(anyhow::anyhow!("Access token retrieval failed: {}", e)))
                    .await;
                return;
            }
        };

        println!("🆔 Getting account ID...");
        let account_id = match get_account_id(&config).await {
            Ok(id) => {
                println!("✅ Account ID retrieved (redacted)");
                id
            }
            Err(e) => {
                println!("❌ Account ID retrieval failed: {}", e);
                let _ = tx
                    .send(Err(anyhow::anyhow!("Account ID retrieval failed: {}", e)))
                    .await;
                return;
            }
        };

        // Try the exact URL that working codex uses: base + codex + responses
        println!(
            "🌐 Making request to ChatGPT Responses API: {} (client {})",
            CHATGPT_CODEX_RESPONSES_URL, CODEX_CLI_VERSION
        );

        // CRITICAL: Use exact headers for ChatGPT Plus plan
        let session_id = uuid::Uuid::new_v4().to_string();
        println!("🔍 DEBUG - Auth and session headers prepared (redacted)");

        let response = match build_codex_request(
            &client,
            &access_token,
            &account_id,
            &session_id,
            &payload,
        )
        .send()
        .await
        {
            Ok(resp) => {
                println!("✅ Got response with status: {}", resp.status());

                if resp.status().is_success() {
                    resp
                } else {
                    // Keep the raw body in memory only long enough to derive a
                    // bounded client-safe summary. Never log or return the
                    // complete upstream JSON.
                    let status = resp.status();
                    let response_body = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Failed to read response body".to_string());
                    println!("❌ Failed with status: {}", status);
                    println!("🔍 DEBUG - Upstream error body redacted from logs");

                    // Send properly formatted error response as SSE
                    // Transform specific error messages for better user experience
                    let user_friendly_message = if status.as_u16() == 429
                        && response_body.contains("usage_limit_reached")
                    {
                        "You've hit your usage limit. Upgrade to Pro (https://openai.com/chatgpt/pricing), or wait for limits to reset (every 5h and every week.).".to_string()
                    } else {
                        upstream_error_message(status, &response_body)
                    };

                    let error_event = ResponseEvent {
                        id: format!("error-{}", uuid::Uuid::new_v4()),
                        object: "chat.completion.chunk".to_string(),
                        created: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        model: request.model.clone(),
                        choices: vec![ResponseChoice {
                            index: 0,
                            delta: ResponseDelta {
                                role: Some("assistant".to_string()),
                                content: Some(user_friendly_message),
                                tool_calls: None,
                            },
                            finish_reason: Some("error".to_string()),
                        }],
                    };
                    let _ = tx.send(Ok(error_event)).await;
                    return;
                }
            }
            Err(e) => {
                println!("❌ Request failed: {}", e);
                let _ = tx.send(Err(anyhow::anyhow!("Request failed: {}", e))).await;
                return;
            }
        };

        // Handle streaming response with proper SSE buffering
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        // Deduplication: Track last sent content
        let mut last_sent_content: Option<String> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let error_event = ResponseEvent {
                        id: format!("error-{}", uuid::Uuid::new_v4()),
                        object: "chat.completion.chunk".to_string(),
                        created: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        model: request.model.clone(),
                        choices: vec![ResponseChoice {
                            index: 0,
                            delta: ResponseDelta {
                                role: Some("assistant".to_string()),
                                content: Some(format!("Stream error: {}", e)),
                                tool_calls: None,
                            },
                            finish_reason: Some("error".to_string()),
                        }],
                    };
                    let _ = tx.send(Ok(error_event)).await;
                    break;
                }
            };

            let chunk_str = match String::from_utf8(chunk.to_vec()) {
                Ok(s) => s,
                Err(e) => {
                    // Try to recover by using lossy UTF-8 conversion
                    let lossy_str = String::from_utf8_lossy(&chunk);
                    println!("⚠️  UTF-8 error: {}, using lossy conversion", e);
                    lossy_str.to_string()
                }
            };

            // Add chunk to buffer
            buffer.push_str(&chunk_str);

            // Process complete lines from buffer
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim_end_matches('\r').to_string();
                buffer = buffer[line_end + 1..].to_string();

                // Skip empty lines (SSE format requirement)
                if line.is_empty() {
                    continue;
                }

                // Process SSE data lines
                if line.starts_with("data: ") {
                    let json_str = line[6..].trim(); // Remove "data: " prefix

                    // Skip "[DONE]" marker
                    if json_str == "[DONE]" {
                        println!("🏁 Received [DONE] marker, ending stream");
                        return;
                    }

                    // Skip empty data lines
                    if json_str.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<Value>(json_str) {
                        Ok(event_json) => {
                            println!("📡 SSE event type: {}", safe_event_type(&event_json));
                            // Convert to our ResponseEvent format
                            if let Some(response_event) = parse_sse_event(&event_json) {
                                // Deduplication logic
                                let mut should_send = true;
                                // Try to extract content from the event
                                let content = response_event
                                    .choices
                                    .get(0)
                                    .and_then(|choice| choice.delta.content.as_ref())
                                    .map(|s| s.trim().to_string());
                                // Only deduplicate non-empty content messages
                                if let Some(ref new_content) = content {
                                    if let Some(ref last_content) = last_sent_content {
                                        if !new_content.is_empty() && new_content == last_content {
                                            should_send = false;
                                        }
                                    }
                                }
                                if should_send {
                                    // Update last sent content if this is a non-empty message
                                    if let Some(ref new_content) = content {
                                        if !new_content.is_empty() {
                                            last_sent_content = Some(new_content.clone());
                                        }
                                    }
                                    if tx.send(Ok(response_event)).await.is_err() {
                                        // Channel closed, stop processing
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("⚠️  JSON parse error in upstream SSE event: {}", e);
                            // Send a structured error response for malformed JSON
                            let error_event = ResponseEvent {
                                id: format!("error-{}", uuid::Uuid::new_v4()),
                                object: "chat.completion.chunk".to_string(),
                                created: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64,
                                model: request.model.clone(),
                                choices: vec![ResponseChoice {
                                    index: 0,
                                    delta: ResponseDelta {
                                        role: Some("assistant".to_string()),
                                        content: Some(format!("JSON parse error: {}", e)),
                                        tool_calls: None,
                                    },
                                    finish_reason: Some("error".to_string()),
                                }],
                            };
                            let _ = tx.send(Ok(error_event)).await;
                            continue;
                        }
                    }
                }
            }
        }
    });

    Ok(rx)
}

fn parse_sse_event(event: &Value) -> Option<ResponseEvent> {
    let event_type = event.get("type").and_then(Value::as_str);
    if event_type.is_some_and(|event_type| event_type.starts_with("response.web_search_call.")) {
        return None;
    }

    // Hosted search is completed by OpenAI. It is observability, not a client
    // function call, so never translate it into a tool_call for Praxis.
    if matches!(
        event_type,
        Some("response.output_item.added" | "response.output_item.done")
    ) && event
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("web_search_call")
    {
        return None;
    }

    // Streaming Responses API: a finished output item. A function_call item must be surfaced as an
    // OpenAI tool_calls delta — the backend streams tool calls here (response.output_item.done). The
    // old code only scanned response.completed's output[] (function_call may not even be there), so
    // tool calls never reached the client — the model would narrate the call but never make it.
    if event_type == Some("response.output_item.done") {
        if let Some(item) = event.get("item") {
            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                let call_id = item
                    .get("call_id")
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
                return Some(ResponseEvent {
                    id: format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8]),
                    object: "chat.completion.chunk".to_string(),
                    created: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64,
                    model: "gpt-4".to_string(),
                    choices: vec![ResponseChoice {
                        index: 0,
                        delta: ResponseDelta {
                            role: Some("assistant".to_string()),
                            content: None,
                            tool_calls: Some(serde_json::json!([{
                                "id": call_id,
                                "type": "function",
                                "index": 0,
                                "function": { "name": name, "arguments": arguments }
                            }])),
                        },
                        finish_reason: Some("tool_calls".to_string()),
                    }],
                });
            }
        }
        // A non-function item (e.g. the assistant message) carries its text in response.completed.
        return None;
    }

    // Try to extract content from various possible structures
    let content = extract_content_from_chatgpt_response(event);
    let model = event
        .get("model")
        .or_else(|| {
            event
                .get("response")
                .and_then(|response| response.get("model"))
        })
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-4")
        .to_string();

    // Create OpenAI-compatible response
    Some(ResponseEvent {
        id: event
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8])),
        object: "chat.completion.chunk".to_string(),
        created: event
            .get("created")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            }),
        model,
        choices: if let Some(content) = content {
            vec![ResponseChoice {
                index: 0,
                delta: ResponseDelta {
                    role: Some("assistant".to_string()),
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason: event
                    .get("finish_reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }]
        } else {
            // Check if this is a finish event
            if event.get("finish_reason").is_some() {
                vec![ResponseChoice {
                    index: 0,
                    delta: ResponseDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: event
                        .get("finish_reason")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                }]
            } else {
                vec![]
            }
        },
    })
}

pub fn aggregate_chat_completion(requested_model: &str, events: &[ResponseEvent]) -> Result<Value> {
    if events.is_empty() {
        anyhow::bail!("upstream stream ended without completion events");
    }

    let mut id = String::new();
    let mut created = 0_i64;
    let mut model = requested_model.to_string();
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut seen_tool_calls = HashSet::new();
    let mut finish_reason = None;

    for event in events {
        if id.is_empty() && !event.id.is_empty() {
            id = event.id.clone();
        }
        if created == 0 && event.created > 0 {
            created = event.created;
        }
        if !event.model.is_empty() && event.model != "gpt-4" {
            model = event.model.clone();
        }

        for choice in &event.choices {
            if let Some(fragment) = &choice.delta.content {
                content.push_str(fragment);
            }
            if let Some(calls) = &choice.delta.tool_calls {
                let values = calls
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![calls.clone()]);
                for mut call in values {
                    if let Some(object) = call.as_object_mut() {
                        object.remove("index");
                    }
                    let identity = call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| call.to_string());
                    if seen_tool_calls.insert(identity) {
                        tool_calls.push(call);
                    }
                }
            }
            if let Some(reason) = &choice.finish_reason {
                if reason == "error" {
                    anyhow::bail!("upstream stream reported an error completion");
                }
                finish_reason = Some(reason.clone());
            }
        }
    }

    if id.is_empty() {
        id = format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    }
    if created == 0 {
        created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
    }

    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() { Value::Null } else { Value::String(content) },
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let finish_reason = finish_reason.unwrap_or_else(|| {
        if message.get("tool_calls").is_some() {
            "tool_calls".to_string()
        } else {
            "stop".to_string()
        }
    });

    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
    }))
}

fn safe_event_type(event: &Value) -> &str {
    event
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
        .unwrap_or("unknown")
}

fn extract_content_from_chatgpt_response(event: &Value) -> Option<String> {
    // Try multiple possible paths for content in ChatGPT's response format
    if let Some(response) = event.get("response") {
        if let Some(content) = extract_responses_message(response) {
            return Some(content);
        }
    }

    // Handle tool calls specifically
    if let Some(response) = event.get("response") {
        if let Some(output) = response.get("output").and_then(|o| o.as_array()) {
            // Look for function_call items in the output
            for item in output {
                if let Some(item_obj) = item.as_object() {
                    if let Some(item_type) = item_obj.get("type").and_then(|t| t.as_str()) {
                        // Tool calls are surfaced from response.output_item.done above — skip them
                        // here (don't bail out, or a function_call before the message would swallow
                        // the assistant's text).
                        if item_type == "function_call" {
                            continue;
                        }
                        // Handle message type items that might contain tool results
                        else if item_type == "message" {
                            if let Some(content) = item_obj.get("content").and_then(|c| {
                                c.as_array().and_then(|arr| {
                                    arr.first().and_then(|first| {
                                        first.get("text").and_then(|t| t.as_str())
                                    })
                                })
                            }) {
                                return Some(content.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Standard OpenAI format
    if let Some(choices) = event.get("choices").and_then(|c| c.as_array()) {
        if let Some(choice) = choices.first() {
            if let Some(delta) = choice.get("delta") {
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    return Some(content.to_string());
                }
            }
            if let Some(message) = choice.get("message") {
                if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                    return Some(content.to_string());
                }
            }
        }
    }

    // ChatGPT Responses API format - direct content field
    if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
        return Some(content.to_string());
    }

    // ChatGPT Responses API format - message field
    if let Some(message) = event.get("message") {
        if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(content) = message.as_str() {
            return Some(content.to_string());
        }
    }

    // ChatGPT Responses API format - response field
    if let Some(response) = event.get("response") {
        if let Some(content) = response.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(content) = response.as_str() {
            return Some(content.to_string());
        }
    }

    // ChatGPT Responses API format - text field
    if let Some(text) = event.get("text").and_then(|t| t.as_str()) {
        return Some(text.to_string());
    }

    // ChatGPT Responses API format - delta field
    if let Some(delta) = event.get("delta") {
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
            return Some(text.to_string());
        }
    }

    None
}

fn extract_responses_message(response: &Value) -> Option<String> {
    let output = response.get("output").and_then(Value::as_array)?;
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }

        let parts = item.get("content").and_then(Value::as_array)?;
        let mut text_parts = Vec::new();
        let mut citations = Vec::new();
        let mut seen_urls = HashSet::new();

        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                text_parts.push(text);
            }
            let Some(annotations) = part.get("annotations").and_then(Value::as_array) else {
                continue;
            };
            for annotation in annotations {
                let Some((url, title)) = visible_url_citation(annotation) else {
                    continue;
                };
                if citations.len() < MAX_VISIBLE_CITATIONS && seen_urls.insert(url.clone()) {
                    citations.push((url, title));
                }
            }
        }

        if text_parts.is_empty() {
            continue;
        }
        let mut text = text_parts.join("\n");
        if !citations.is_empty() {
            text.push_str("\n\nИсточники:");
            for (url, title) in citations {
                text.push_str("\n- ");
                if let Some(title) = title {
                    text.push_str(&title);
                    text.push_str(" — ");
                }
                text.push_str(&url);
            }
        }
        return Some(text);
    }

    None
}

fn visible_url_citation(annotation: &Value) -> Option<(String, Option<String>)> {
    let citation = if annotation.get("type").and_then(Value::as_str) == Some("url_citation") {
        annotation.get("url_citation").unwrap_or(annotation)
    } else {
        annotation.get("url_citation")?
    };
    let raw_url = citation.get("url").and_then(Value::as_str)?;
    if raw_url.len() > 8 * 1024 {
        return None;
    }
    let parsed = Url::parse(raw_url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }

    let title = citation
        .get("title")
        .and_then(Value::as_str)
        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(160).collect());
    Some((parsed.to_string(), title))
}

fn upstream_error_message(status: reqwest::StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error = parsed
        .as_ref()
        .and_then(|value| value.get("error").or(Some(value)));

    let code = error
        .and_then(|value| value.get("code").or_else(|| value.get("type")))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        });
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .and_then(bounded_error_text);

    match (code, message) {
        (Some(code), Some(message)) => {
            format!("Upstream error {status} ({code}): {message}")
        }
        (Some(code), None) => format!("Upstream error {status} ({code})"),
        (None, Some(message)) => format!("Upstream error {status}: {message}"),
        (None, None) => format!("Upstream error {status}"),
    }
}

fn bounded_error_text(raw: &str) -> Option<String> {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }

    let mut chars = normalized.chars();
    let mut bounded: String = chars.by_ref().take(MAX_UPSTREAM_ERROR_CHARS).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    Some(bounded)
}

async fn get_access_token(config: &Config) -> Result<String> {
    use crate::login::lib::CodexAuth;

    let auth = CodexAuth::from_auth_dir(&config.codex_home)?
        .ok_or_else(|| anyhow::anyhow!("No authentication found"))?;

    let token_data = auth.get_token_data().await?;
    Ok(token_data.access_token)
}

async fn get_account_id(config: &Config) -> Result<String> {
    use crate::login::lib::CodexAuth;

    let auth = CodexAuth::from_auth_dir(&config.codex_home)?
        .ok_or_else(|| anyhow::anyhow!("No authentication found"))?;

    let token_data = auth.get_token_data().await?;
    token_data
        .account_id
        .ok_or_else(|| anyhow::anyhow!("No account ID found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translates_text_and_image_url_parts_to_responses_content() {
        let request: ChatRequest = serde_json::from_value(json!({
            "model": "gpt-5.5",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/photo.jpg",
                            "detail": "high"
                        }
                    }
                ]
            }]
        }))
        .unwrap();
        request.validate_content().unwrap();

        assert_eq!(
            build_responses_input(&request.messages),
            vec![json!({
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "What is this?"},
                    {
                        "type": "input_image",
                        "image_url": "https://example.com/photo.jpg",
                        "detail": "high"
                    }
                ]
            })]
        );
    }

    #[test]
    fn preserves_legacy_string_null_and_tool_loop_mapping() {
        let request: ChatRequest = serde_json::from_value(json!({
            "model": "gpt-5.5",
            "messages": [
                {"role": "system", "content": "rules"},
                {"role": "user", "content": "hello"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"q\":1}"}
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "done"
                }
            ]
        }))
        .unwrap();
        request.validate_content().unwrap();

        assert_eq!(
            build_responses_input(&request.messages),
            vec![
                json!({"role": "user", "content": "<system>\nrules\n</system>"}),
                json!({"role": "user", "content": "hello"}),
                json!({
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup",
                    "arguments": "{\"q\":1}"
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "done"
                })
            ]
        );
    }

    #[test]
    fn omits_image_detail_when_client_omits_it() {
        let request: ChatRequest = serde_json::from_value(json!({
            "model": "gpt-5.5",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "https://example.com/photo.jpg"}
                }]
            }]
        }))
        .unwrap();

        let input = build_responses_input(&request.messages);
        assert!(input[0]["content"][0].get("detail").is_none());
    }

    #[test]
    fn maps_hosted_search_without_unsupported_max_tool_calls() {
        let request: ChatRequest = serde_json::from_value(json!({
            "model": "gpt-5.6-sol",
            "messages": [{"role": "user", "content": "latest release"}],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "web_read",
                        "description": "Read a URL",
                        "parameters": {"type": "object"},
                        "strict": false
                    }
                },
                {
                    "type": "web_search",
                    "external_web_access": true,
                    "search_context_size": "medium",
                    "max_uses": 3
                }
            ]
        }))
        .unwrap();
        request.validate_content().unwrap();

        let payload = build_responses_payload(
            &request,
            "instructions".to_string(),
            build_responses_input(&request.messages),
        );
        assert_eq!(payload["model"], "gpt-5.6-sol");
        assert!(payload.get("max_tool_calls").is_none());
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["parallel_tool_calls"], false);
        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["strict"], false);
        assert_eq!(payload["tools"][1]["type"], "web_search");
        assert_eq!(payload["tools"][1]["external_web_access"], true);
        assert_eq!(payload["tools"][1]["search_context_size"], "medium");
        assert!(payload["tools"][1].get("max_uses").is_none());
    }

    #[test]
    fn omitted_function_strictness_stays_non_strict_for_chat_compatibility() {
        let request: ChatRequest = serde_json::from_value(json!({
            "model": "gpt-5.6-terra",
            "messages": [{"role": "user", "content": "inspect"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "browse_page",
                    "description": "Browse a page",
                    "parameters": {
                        "type": "object",
                        "properties": {"url": {"type": "string"}}
                    }
                }
            }]
        }))
        .unwrap();

        let payload = build_responses_payload(
            &request,
            "instructions".to_string(),
            build_responses_input(&request.messages),
        );

        assert_eq!(payload["tools"][0]["strict"], false);
        assert!(payload["tools"][0]["parameters"]
            .get("additionalProperties")
            .is_none());
    }

    #[test]
    fn codex_request_has_version_gated_user_agent() {
        let request = build_codex_request(
            &Client::new(),
            "test-access-token",
            "test-account",
            "test-session",
            &json!({"model": "gpt-5.6-luna"}),
        )
        .build()
        .unwrap();

        assert_eq!(CODEX_USER_AGENT, "codex_cli_rs/0.144.0");
        assert_eq!(CODEX_CLI_VERSION, "0.144.0");
        assert_eq!(request.url().as_str(), CHATGPT_CODEX_RESPONSES_URL);
        assert_eq!(
            request.headers().get(reqwest::header::USER_AGENT).unwrap(),
            CODEX_USER_AGENT
        );
        assert_eq!(
            request.headers().get("originator").unwrap(),
            CODEX_ORIGINATOR
        );
    }

    #[test]
    fn hosted_search_events_never_become_client_tool_calls() {
        for event in [
            json!({"type": "response.web_search_call.in_progress"}),
            json!({
                "type": "response.output_item.added",
                "item": {"type": "web_search_call", "id": "ws_1"}
            }),
            json!({
                "type": "response.output_item.done",
                "item": {"type": "web_search_call", "id": "ws_1"}
            }),
        ] {
            assert!(parse_sse_event(&event).is_none());
        }
    }

    #[test]
    fn completed_search_response_emits_citations_and_actual_model() {
        let event = json!({
            "type": "response.completed",
            "response": {
                "model": "gpt-5.6-terra",
                "output": [
                    {"type": "web_search_call", "id": "ws_1", "status": "completed"},
                    {
                        "type": "message",
                        "content": [{
                            "type": "output_text",
                            "text": "Current answer",
                            "annotations": [{
                                "type": "url_citation",
                                "url": "https://example.com/source",
                                "title": "Primary source"
                            }]
                        }]
                    }
                ]
            }
        });

        let translated = parse_sse_event(&event).unwrap();
        assert_eq!(translated.model, "gpt-5.6-terra");
        let delta = &translated.choices[0].delta;
        assert_eq!(delta.tool_calls, None);
        assert_eq!(
            delta.content.as_deref(),
            Some("Current answer\n\nИсточники:\n- Primary source — https://example.com/source")
        );
    }

    #[test]
    fn response_citations_are_visible_deduplicated_and_capped() {
        let mut annotations = vec![json!({
            "type": "url_citation",
            "url": "ftp://unsafe.example/ignored",
            "title": "unsafe"
        })];
        for index in 0..14 {
            let citation = json!({
                "url": format!("https://source.example/{index}"),
                "title": if index == 0 { "Source\n 0" } else { "Source" }
            });
            if index == 1 {
                annotations.push(json!({
                    "type": "url_citation",
                    "url_citation": citation
                }));
                annotations.push(json!({
                    "type": "url_citation",
                    "url": "https://source.example/1",
                    "title": "duplicate"
                }));
            } else {
                annotations.push(json!({
                    "type": "url_citation",
                    "url": citation["url"],
                    "title": citation["title"]
                }));
            }
        }
        let response = json!({
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "Answer already mentions https://source.example/0",
                    "annotations": annotations
                }]
            }]
        });

        let text = extract_responses_message(&response).unwrap();
        let source_lines: Vec<_> = text.lines().filter(|line| line.starts_with("- ")).collect();
        assert_eq!(source_lines.len(), MAX_VISIBLE_CITATIONS);
        assert_eq!(
            source_lines
                .iter()
                .filter(|line| line.ends_with("source.example/1"))
                .count(),
            1
        );
        assert!(text.contains("Source 0 — https://source.example/0"));
        assert!(text.contains("https://source.example/11"));
        assert!(!text.contains("https://source.example/12"));
        assert!(!text.contains("ftp://unsafe.example"));
    }

    #[test]
    fn non_stream_completion_aggregates_text_for_openai_sdk() {
        let events = vec![
            ResponseEvent {
                id: "chatcmpl-test".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 123,
                model: "gpt-5.6-terra".to_string(),
                choices: vec![ResponseChoice {
                    index: 0,
                    delta: ResponseDelta {
                        role: Some("assistant".to_string()),
                        content: Some("O".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
            },
            ResponseEvent {
                id: "chatcmpl-test".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 123,
                model: "gpt-5.6-terra".to_string(),
                choices: vec![ResponseChoice {
                    index: 0,
                    delta: ResponseDelta {
                        role: None,
                        content: Some("K".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
            },
        ];

        let completion = aggregate_chat_completion("gpt-5.6-terra", &events).unwrap();
        assert_eq!(completion["object"], "chat.completion");
        assert_eq!(completion["model"], "gpt-5.6-terra");
        assert_eq!(completion["choices"][0]["message"]["content"], "OK");
        assert_eq!(completion["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn non_stream_completion_preserves_tool_calls() {
        let events = vec![ResponseEvent {
            id: "chatcmpl-tool".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 456,
            model: "gpt-5.6-terra".to_string(),
            choices: vec![ResponseChoice {
                index: 0,
                delta: ResponseDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                    tool_calls: Some(json!([{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}
                    }])),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
        }];

        let completion = aggregate_chat_completion("gpt-5.6-terra", &events).unwrap();
        let call = &completion["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(completion["choices"][0]["message"]["content"], Value::Null);
        assert_eq!(completion["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "read_file");
        assert!(call.get("index").is_none());
    }

    #[test]
    fn non_stream_completion_rejects_error_event() {
        let events = vec![ResponseEvent {
            id: "chatcmpl-error".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 789,
            model: "gpt-5.6-terra".to_string(),
            choices: vec![ResponseChoice {
                index: 0,
                delta: ResponseDelta {
                    role: Some("assistant".to_string()),
                    content: Some("upstream failed".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("error".to_string()),
            }],
        }];

        assert!(aggregate_chat_completion("gpt-5.6-terra", &events).is_err());
    }

    #[test]
    fn logs_only_bounded_event_labels_and_client_safe_upstream_errors() {
        assert_eq!(
            safe_event_type(&json!({"type": "response.output_text.delta"})),
            "response.output_text.delta"
        );
        assert_eq!(safe_event_type(&json!({"type": "bad\nprivate"})), "unknown");

        let long_message = format!("invalid\nrequest {}", "x".repeat(700));
        let body = json!({
            "error": {
                "code": "invalid_request_error",
                "message": long_message,
                "private_request_echo": "must-not-leak"
            },
            "private": "must-not-leak-either"
        })
        .to_string();
        let message = upstream_error_message(reqwest::StatusCode::BAD_REQUEST, &body);
        assert!(message.contains("invalid_request_error"));
        assert!(!message.contains('\n'));
        assert!(!message.contains("must-not-leak"));
        assert!(message.chars().count() <= MAX_UPSTREAM_ERROR_CHARS + 80);

        assert_eq!(
            upstream_error_message(reqwest::StatusCode::BAD_GATEWAY, "raw private response"),
            "Upstream error 502 Bad Gateway"
        );
    }
}
