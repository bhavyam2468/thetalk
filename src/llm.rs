use crate::attach::{
    build_attachment_system_prompt, format_user_turn_text, rules, AttachmentKind,
    AttachmentPayload,
};
use crate::config::{ApiFormat, AppConfig};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
    Arc,
};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ChatTurnMsg {
    pub role: String,
    pub content: String,
    pub attachments: Vec<AttachmentPayload>,
}

impl ChatTurnMsg {
    #[allow(dead_code)]
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    ModelsFetched(Result<Vec<String>, String>),
    ModelAutoSelected { req_id: u64, model: String },
    Delta { req_id: u64, full_raw: String },
    Finished { req_id: u64, full_raw: String },
    Error { req_id: u64, message: String },
}

/// Merges consecutive messages of the same role so strict servers
/// (llama.cpp, vLLM, Gemini, Anthropic, mistral.rs) never reject history.
pub fn normalize_messages(msgs: &[ChatTurnMsg]) -> Vec<ChatTurnMsg> {
    let mut out: Vec<ChatTurnMsg> = Vec::with_capacity(msgs.len());
    for m in msgs {
        if m.content.trim().is_empty() && m.attachments.is_empty() {
            continue;
        }
        if let Some(prev) = out.last_mut() {
            if prev.role == m.role {
                if !m.content.trim().is_empty() {
                    if !prev.content.trim().is_empty() {
                        prev.content.push_str("\n\n");
                    }
                    prev.content.push_str(&m.content);
                }
                prev.attachments.extend(m.attachments.clone());
                continue;
            }
        }
        out.push(m.clone());
    }
    out
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout_read(Duration::from_secs(180))
        .build()
}

pub fn fetch_models_async(
    cfg: AppConfig,
    tx: Sender<LlmEvent>,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    std::thread::spawn(move || {
        let res = fetch_models_blocking(&cfg);
        let _ = tx.send(LlmEvent::ModelsFetched(res));
        if let Some(wake) = notify {
            wake();
        }
    });
}

fn models_url(cfg: &AppConfig) -> String {
    let base = cfg.normalized_base_url();
    match cfg.active_format() {
        ApiFormat::OpenAi => format!("{}/models", base),
        ApiFormat::Anthropic => {
            if base.ends_with("/v1") {
                format!("{}/models", base)
            } else if base.contains("api.anthropic.com") {
                format!("{}/v1/models", base)
            } else {
                format!("{}/models", base)
            }
        }
    }
}

pub fn fetch_models_blocking(cfg: &AppConfig) -> Result<Vec<String>, String> {
    let agent = build_agent();
    let url = models_url(cfg);
    let mut req = agent.get(&url).set("Accept", "application/json");
    let key = cfg.api_key.trim();
    match cfg.active_format() {
        ApiFormat::OpenAi => {
            if !key.is_empty() {
                req = req.set("Authorization", &format!("Bearer {}", key));
            }
        }
        ApiFormat::Anthropic => {
            req = req.set("anthropic-version", "2023-06-01");
            if !key.is_empty() {
                req = req.set("x-api-key", key);
            }
        }
    }

    let resp = match req.call() {
        Ok(r) => r,
        Err(e) => {
            // If Anthropic has no API key set yet, return curated model IDs instead of failing silently
            if cfg.active_format() == ApiFormat::Anthropic && key.is_empty() {
                return Ok(vec![
                    "claude-sonnet-4-5".to_string(),
                    "claude-opus-4-5".to_string(),
                    "claude-haiku-4-5".to_string(),
                ]);
            }
            return Err(format_ureq_err(e));
        }
    };
    let json: Value = resp
        .into_json()
        .map_err(|e| format!("Invalid /models JSON: {}", e))?;

    let mut models = Vec::new();
    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                models.push(id.to_string());
            }
        }
    } else if let Some(arr) = json.get("models").and_then(|d| d.as_array()) {
        // Native Ollama `/api/tags`-style fallback if user omitted `/v1`
        for item in arr {
            if let Some(name) = item
                .get("name")
                .or_else(|| item.get("model"))
                .and_then(|v| v.as_str())
            {
                models.push(name.to_string());
            }
        }
    }
    Ok(models)
}

/// Builds the JSON payload for either OpenAI `/chat/completions` or Anthropic `/messages`.
pub fn build_request_payload(
    cfg: &AppConfig,
    model: &str,
    messages: &[ChatTurnMsg],
    stream: bool,
) -> Value {
    let normalized = normalize_messages(messages);
    let all_attachments: Vec<&AttachmentPayload> = normalized
        .iter()
        .flat_map(|m| m.attachments.iter())
        .collect();
    let system_prompt =
        build_attachment_system_prompt(&all_attachments, cfg.vision, cfg.active_format());

    // Keep heavy base64 visual payloads on the most recent user message with attachments
    // so multi-turn conversations stay responsive and within context budgets.
    let last_att_idx = normalized.iter().rposition(|m| !m.attachments.is_empty());

    match cfg.active_format() {
        ApiFormat::OpenAi => {
            let mut api_msgs: Vec<Value> = Vec::with_capacity(normalized.len() + 1);
            if let Some(sys) = system_prompt {
                api_msgs.push(json!({ "role": "system", "content": sys }));
            }
            for (idx, m) in normalized.iter().enumerate() {
                if m.role == "user" && !m.attachments.is_empty() {
                    let text = format_user_turn_text(&m.content, &m.attachments, cfg.vision);
                    let include_binary = Some(idx) == last_att_idx && cfg.vision;
                    if include_binary {
                        let mut parts: Vec<Value> = vec![json!({ "type": "text", "text": text })];
                        let mut img_count = 0usize;
                        for att in &m.attachments {
                            if att.kind == AttachmentKind::Image {
                                if img_count < rules::MAX_IMAGES {
                                    if let Some(b64) = &att.base64_data {
                                        parts.push(json!({
                                            "type": "image_url",
                                            "image_url": { "url": format!("data:{};base64,{}", att.mime, b64) }
                                        }));
                                        img_count += 1;
                                    }
                                }
                            } else if att.kind == AttachmentKind::Pdf {
                                for (pmime, pb64) in &att.page_images {
                                    if img_count >= rules::MAX_IMAGES {
                                        break;
                                    }
                                    parts.push(json!({
                                        "type": "image_url",
                                        "image_url": { "url": format!("data:{};base64,{}", pmime, pb64) }
                                    }));
                                    img_count += 1;
                                }
                            }
                        }
                        if parts.len() > 1 {
                            api_msgs.push(json!({ "role": "user", "content": parts }));
                            continue;
                        }
                    }
                    api_msgs.push(json!({ "role": "user", "content": text }));
                } else {
                    api_msgs.push(json!({ "role": m.role, "content": m.content }));
                }
            }
            json!({
                "model": model,
                "messages": api_msgs,
                "stream": stream,
            })
        }
        ApiFormat::Anthropic => {
            let mut api_msgs: Vec<Value> = Vec::with_capacity(normalized.len());
            for (idx, m) in normalized.iter().enumerate() {
                if m.role == "user" && !m.attachments.is_empty() {
                    let text = format_user_turn_text(&m.content, &m.attachments, cfg.vision);
                    let include_binary = Some(idx) == last_att_idx;
                    if include_binary {
                        let mut blocks: Vec<Value> = Vec::new();
                        let mut img_count = 0usize;
                        for att in &m.attachments {
                            if att.kind == AttachmentKind::Image && cfg.vision {
                                if img_count < rules::MAX_IMAGES {
                                    if let Some(b64) = &att.base64_data {
                                        blocks.push(json!({
                                            "type": "image",
                                            "source": {
                                                "type": "base64",
                                                "media_type": att.mime,
                                                "data": b64,
                                            }
                                        }));
                                        img_count += 1;
                                    }
                                }
                            } else if att.kind == AttachmentKind::Pdf {
                                if let Some(b64) = &att.base64_data {
                                    blocks.push(json!({
                                        "type": "document",
                                        "source": {
                                            "type": "base64",
                                            "media_type": "application/pdf",
                                            "data": b64,
                                        }
                                    }));
                                } else if cfg.vision {
                                    for (pmime, pb64) in &att.page_images {
                                        if img_count >= rules::MAX_IMAGES {
                                            break;
                                        }
                                        blocks.push(json!({
                                            "type": "image",
                                            "source": {
                                                "type": "base64",
                                                "media_type": pmime,
                                                "data": pb64,
                                            }
                                        }));
                                        img_count += 1;
                                    }
                                }
                            }
                        }
                        blocks.push(json!({ "type": "text", "text": text }));
                        api_msgs.push(json!({ "role": "user", "content": blocks }));
                        continue;
                    }
                    api_msgs.push(json!({ "role": "user", "content": text }));
                } else {
                    api_msgs.push(json!({ "role": m.role, "content": m.content }));
                }
            }
            let mut body = json!({
                "model": model,
                "max_tokens": 8192,
                "messages": api_msgs,
                "stream": stream,
            });
            if let Some(sys) = system_prompt {
                body["system"] = Value::String(sys);
            }
            body
        }
    }
}

fn chat_endpoint_url(cfg: &AppConfig) -> String {
    let base = cfg.normalized_base_url();
    match cfg.active_format() {
        ApiFormat::OpenAi => {
            if base.ends_with("/chat/completions") {
                base
            } else {
                format!("{}/chat/completions", base)
            }
        }
        ApiFormat::Anthropic => {
            if base.ends_with("/messages") {
                base
            } else if base.ends_with("/v1") {
                format!("{}/messages", base)
            } else if base.contains("api.anthropic.com") {
                format!("{}/v1/messages", base)
            } else {
                format!("{}/messages", base)
            }
        }
    }
}

pub fn spawn_chat_stream(
    req_id: u64,
    cfg: AppConfig,
    messages: Vec<ChatTurnMsg>,
    cancel: Arc<AtomicBool>,
    tx: Sender<LlmEvent>,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    std::thread::spawn(move || {
        let wake = || {
            if let Some(ref w) = notify {
                w();
            }
        };

        let mut model = cfg.model.trim().to_string();
        if model.is_empty() {
            match fetch_models_blocking(&cfg) {
                Ok(list) if !list.is_empty() => {
                    model = list[0].clone();
                    let _ = tx.send(LlmEvent::ModelAutoSelected {
                        req_id,
                        model: model.clone(),
                    });
                    wake();
                }
                Ok(_) => {
                    let _ = tx.send(LlmEvent::Error {
                        req_id,
                        message: "No models loaded on server — set a model in /settings"
                            .to_string(),
                    });
                    wake();
                    return;
                }
                Err(err) => {
                    let _ = tx.send(LlmEvent::Error {
                        req_id,
                        message: err,
                    });
                    wake();
                    return;
                }
            }
        }

        let url = chat_endpoint_url(&cfg);
        let payload = build_request_payload(&cfg, &model, &messages, true);

        let agent = build_agent();
        let mut req = agent
            .post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream");
        let key = cfg.api_key.trim();
        match cfg.active_format() {
            ApiFormat::OpenAi => {
                if !key.is_empty() {
                    req = req.set("Authorization", &format!("Bearer {}", key));
                }
            }
            ApiFormat::Anthropic => {
                req = req.set("anthropic-version", "2023-06-01");
                if !key.is_empty() {
                    req = req.set("x-api-key", key);
                }
            }
        }

        let resp = match req.send_json(payload) {
            Ok(r) => r,
            Err(e) => {
                if !cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(LlmEvent::Error {
                        req_id,
                        message: format_ureq_err(e),
                    });
                    wake();
                }
                return;
            }
        };

        let content_type = resp.header("content-type").unwrap_or("").to_ascii_lowercase();
        let reader = BufReader::new(resp.into_reader());

        // Handle non-streaming JSON fallback gracefully if an endpoint ignores `stream: true`
        if content_type.contains("application/json") && !content_type.contains("event-stream") {
            if let Ok(val) = serde_json::from_reader::<_, Value>(reader) {
                let text = match cfg.active_format() {
                    ApiFormat::OpenAi => val
                        .pointer("/choices/0/message/content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    ApiFormat::Anthropic => val
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|b| {
                                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        b.get("text").and_then(|t| t.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default(),
                };
                let _ = tx.send(LlmEvent::Finished {
                    req_id,
                    full_raw: text,
                });
                wake();
            }
            return;
        }

        let mut accumulated = String::new();
        let mut in_reasoning_delta = false;

        for line_res in reader.lines() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let line = match line_res {
                Ok(l) => l,
                Err(e) => {
                    if !cancel.load(Ordering::Relaxed) && accumulated.is_empty() {
                        let _ = tx.send(LlmEvent::Error {
                            req_id,
                            message: format!("Stream read interrupted: {}", e),
                        });
                        wake();
                    }
                    break;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(':') {
                continue;
            }
            let Some(data) = trimmed.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }

            if let Some(step) = parse_sse_delta(data) {
                let mut changed = false;
                if let Some(r_tok) = step.reasoning {
                    if !r_tok.is_empty() {
                        if !in_reasoning_delta && accumulated.is_empty() {
                            accumulated.push_str("<think>");
                            in_reasoning_delta = true;
                        }
                        accumulated.push_str(&r_tok);
                        changed = true;
                    }
                }
                if let Some(c_tok) = step.content {
                    if !c_tok.is_empty() {
                        if in_reasoning_delta {
                            accumulated.push_str("</think>\n");
                            in_reasoning_delta = false;
                        }
                        accumulated.push_str(&c_tok);
                        changed = true;
                    }
                }
                if let Some(err_msg) = step.error {
                    let _ = tx.send(LlmEvent::Error {
                        req_id,
                        message: err_msg,
                    });
                    wake();
                    return;
                }
                if changed {
                    let _ = tx.send(LlmEvent::Delta {
                        req_id,
                        full_raw: accumulated.clone(),
                    });
                    wake();
                }
            }
        }

        if in_reasoning_delta {
            accumulated.push_str("</think>\n");
        }
        let _ = tx.send(LlmEvent::Finished {
            req_id,
            full_raw: accumulated,
        });
        wake();
    });
}

#[derive(Default)]
pub struct SseDelta {
    pub reasoning: Option<String>,
    pub content: Option<String>,
    pub error: Option<String>,
}

pub fn parse_sse_delta(json_str: &str) -> Option<SseDelta> {
    let val: Value = serde_json::from_str(json_str).ok()?;
    if let Some(err) = val.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        return Some(SseDelta {
            error: Some(msg),
            ..Default::default()
        });
    }

    // Anthropic SSE format (`content_block_delta`)
    if let Some(ev_type) = val.get("type").and_then(|t| t.as_str()) {
        if ev_type == "content_block_delta" {
            let delta = val.get("delta")?;
            let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if dtype == "thinking_delta" {
                return Some(SseDelta {
                    reasoning: delta
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    ..Default::default()
                });
            }
            if dtype == "text_delta" {
                return Some(SseDelta {
                    content: delta
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    // OpenAI SSE format
    let choice = val.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta").or_else(|| choice.get("message"))?;
    let reasoning = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let content = delta
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(SseDelta {
        reasoning,
        content,
        error: None,
    })
}

fn format_ureq_err(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .or_else(|| v.get("error"))
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| body.chars().take(140).collect());
            if parsed.trim().is_empty() {
                format!("HTTP {}", code)
            } else {
                format!("HTTP {}: {}", code, parsed.trim())
            }
        }
        ureq::Error::Transport(t) => {
            let s = t.to_string();
            if s.contains("Connection refused") {
                "Connection refused — check local LLM server or /settings Base URL".to_string()
            } else {
                format!("Network error: {}", s)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_and_anthropic_sse_deltas() {
        let d1 = parse_sse_delta(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#).unwrap();
        assert_eq!(d1.content.as_deref(), Some("Hello"));
        assert!(d1.reasoning.is_none());

        let d2 = parse_sse_delta(r#"{"choices":[{"delta":{"reasoning_content":"step 1"}}]}"#).unwrap();
        assert_eq!(d2.reasoning.as_deref(), Some("step 1"));

        let a1 = parse_sse_delta(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ponder"}}"#,
        )
        .unwrap();
        assert_eq!(a1.reasoning.as_deref(), Some("ponder"));

        let a2 = parse_sse_delta(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer"}}"#,
        )
        .unwrap();
        assert_eq!(a2.content.as_deref(), Some("Answer"));
    }

    #[test]
    fn normalizes_consecutive_same_role_turns() {
        let msgs = vec![
            ChatTurnMsg::text("user", "first"),
            ChatTurnMsg::text("user", "second"),
            ChatTurnMsg::text("assistant", "reply"),
        ];
        let norm = normalize_messages(&msgs);
        assert_eq!(norm.len(), 2);
        assert_eq!(norm[0].content, "first\n\nsecond");
    }

    #[test]
    fn builds_multimodal_payload_with_system_prompt_for_openai_and_anthropic() {
        let img = AttachmentPayload {
            name: "ui.png".into(),
            path: "/tmp/ui.png".into(),
            mime: "image/png".into(),
            kind: AttachmentKind::Image,
            size_bytes: 512,
            extracted_text: None,
            page_count: None,
            truncated: false,
            base64_data: Some("AQID".into()),
            page_images: Vec::new(),
            dimensions: Some((100, 100)),
            note: None,
        };
        let pdf = AttachmentPayload {
            name: "doc.pdf".into(),
            path: "/tmp/doc.pdf".into(),
            mime: "application/pdf".into(),
            kind: AttachmentKind::Pdf,
            size_bytes: 1024,
            extracted_text: Some("[page 1]\nSample PDF".into()),
            page_count: Some(1),
            truncated: false,
            base64_data: Some("JVBERi0=".into()),
            page_images: Vec::new(),
            dimensions: None,
            note: None,
        };
        let turn = ChatTurnMsg {
            role: "user".into(),
            content: "Summarize both".into(),
            attachments: vec![img, pdf],
        };

        let mut oa_cfg = AppConfig::default();
        oa_cfg.select_profile("openai");
        let oa_payload = build_request_payload(&oa_cfg, "gpt-4o-mini", &[turn.clone()], true);
        assert_eq!(oa_payload["messages"][0]["role"], "system");
        assert!(oa_payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("cite exact page numbers"));
        let oa_parts = oa_payload["messages"][1]["content"].as_array().unwrap();
        assert_eq!(oa_parts[0]["type"], "text");
        assert_eq!(oa_parts[1]["type"], "image_url");

        let mut ant_cfg = AppConfig::default();
        ant_cfg.select_profile("anthropic");
        let ant_payload =
            build_request_payload(&ant_cfg, "claude-sonnet-4-5", &[turn], true);
        assert!(ant_payload["system"]
            .as_str()
            .unwrap()
            .contains("PDF Attachments"));
        let ant_blocks = ant_payload["messages"][0]["content"].as_array().unwrap();
        assert_eq!(ant_blocks[0]["type"], "image");
        assert_eq!(ant_blocks[1]["type"], "document");
        assert_eq!(ant_blocks[2]["type"], "text");
    }
}
