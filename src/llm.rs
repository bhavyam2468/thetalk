use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
    Arc,
};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurnMsg {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    ModelsFetched(Result<Vec<String>, String>),
    ModelAutoSelected { req_id: u64, model: String },
    Delta { req_id: u64, full_raw: String },
    Finished { req_id: u64, full_raw: String },
    Error { req_id: u64, message: String },
}

/// Merges consecutive messages of the same role so strict OpenAI-compatible local servers
/// (llama.cpp, vLLM, Gemini, mistral.rs) never reject history (`final-chat/src/lib/llm.ts`).
pub fn normalize_messages(msgs: &[ChatTurnMsg]) -> Vec<ChatTurnMsg> {
    let mut out: Vec<ChatTurnMsg> = Vec::with_capacity(msgs.len());
    for m in msgs {
        if m.content.trim().is_empty() {
            continue;
        }
        if let Some(prev) = out.last_mut() {
            if prev.role == m.role {
                prev.content.push_str("\n\n");
                prev.content.push_str(&m.content);
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

pub fn fetch_models_async(cfg: AppConfig, tx: Sender<LlmEvent>, notify: Option<Arc<dyn Fn() + Send + Sync>>) {
    std::thread::spawn(move || {
        let res = fetch_models_blocking(&cfg);
        let _ = tx.send(LlmEvent::ModelsFetched(res));
        if let Some(wake) = notify {
            wake();
        }
    });
}

pub fn fetch_models_blocking(cfg: &AppConfig) -> Result<Vec<String>, String> {
    let agent = build_agent();
    let url = format!("{}/models", cfg.normalized_base_url());
    let mut req = agent.get(&url).set("Accept", "application/json");
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", key));
    }

    let resp = req.call().map_err(format_ureq_err)?;
    let json: serde_json::Value = resp.into_json().map_err(|e| format!("Invalid /models JSON: {}", e))?;

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
                        message: "No models loaded on server — set a model in /settings".to_string(),
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

        let normalized = normalize_messages(&messages);
        let base = cfg.normalized_base_url();
        let url = if base.ends_with("/chat/completions") {
            base
        } else {
            format!("{}/chat/completions", base)
        };

        let payload = serde_json::json!({
            "model": model,
            "messages": normalized,
            "stream": true,
        });

        let agent = build_agent();
        let mut req = agent
            .post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream");
        let key = cfg.api_key.trim();
        if !key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", key));
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
            if let Ok(val) = serde_json::from_reader::<_, serde_json::Value>(reader) {
                let text = val
                    .pointer("/choices/0/message/content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
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
struct SseDelta {
    reasoning: Option<String>,
    content: Option<String>,
    error: Option<String>,
}

pub fn parse_sse_delta(json_str: &str) -> Option<SseDelta> {
    let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
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
            let parsed = serde_json::from_str::<serde_json::Value>(&body)
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
    fn parses_openai_and_reasoning_sse_deltas() {
        let d1 = parse_sse_delta(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#).unwrap();
        assert_eq!(d1.content.as_deref(), Some("Hello"));
        assert!(d1.reasoning.is_none());

        let d2 = parse_sse_delta(r#"{"choices":[{"delta":{"reasoning_content":"step 1"}}]}"#).unwrap();
        assert_eq!(d2.reasoning.as_deref(), Some("step 1"));
    }

    #[test]
    fn normalizes_consecutive_same_role_turns() {
        let msgs = vec![
            ChatTurnMsg {
                role: "user".into(),
                content: "first".into(),
            },
            ChatTurnMsg {
                role: "user".into(),
                content: "second".into(),
            },
            ChatTurnMsg {
                role: "assistant".into(),
                content: "reply".into(),
            },
        ];
        let norm = normalize_messages(&msgs);
        assert_eq!(norm.len(), 2);
        assert_eq!(norm[0].content, "first\n\nsecond");
    }
}
