//! Streaming chat completions. Plain SSE over ureq, so any OpenAI-compatible
//! server works — including a local llama.cpp or Ollama with no key at all.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{endpoint_url, resolve_key, Config};

pub enum Msg {
    Chunk(String),
    Error(String),
    Done,
}

/// Runs on a worker thread; every exit path sends exactly one `Done`.
pub fn stream_completion(cfg: &Config, messages: Vec<Value>, out: Sender<Msg>, stop: Arc<AtomicBool>) {
    if let Err(e) = run(cfg, messages, &out, &stop) {
        let _ = out.send(Msg::Error(e));
    }
    let _ = out.send(Msg::Done);
}

fn run(
    cfg: &Config,
    messages: Vec<Value>,
    out: &Sender<Msg>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let api = &cfg.api;
    let key = resolve_key(api).map_err(|e| format!("key_command failed: {e}"))?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        // We want to read the body of a 401 rather than get an opaque error.
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(api.timeout)))
        .build()
        .new_agent();

    let body = json!({
        "model": api.model,
        "messages": messages,
        "stream": true,
        "temperature": api.temperature,
        "max_tokens": api.max_tokens,
    });

    let mut req = agent
        .post(endpoint_url(&api.url))
        .header("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }

    let mut resp = req.send_json(&body).map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let detail = resp.body_mut().read_to_string().unwrap_or_default();
        let detail: String = detail.chars().take(400).collect();
        return Err(format!("HTTP {status}: {detail}"));
    }

    for line in BufReader::new(resp.body_mut().as_reader()).lines() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let line = line.map_err(|e| e.to_string())?;
        let Some(payload) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            break;
        }
        let Ok(obj) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let chunk = obj["choices"]
            .get(0)
            .and_then(|c| c["delta"]["content"].as_str())
            .unwrap_or("");
        if !chunk.is_empty() && out.send(Msg::Chunk(chunk.to_string())).is_err() {
            break; // UI is gone
        }
    }
    Ok(())
}
