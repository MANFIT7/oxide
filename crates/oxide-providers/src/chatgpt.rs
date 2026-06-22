//! ChatGPT-subscription provider — no API key, no codex CLI subprocess.
//!
//! Reuses the OAuth token the `codex` CLI already stored at `~/.codex/auth.json`
//! (ChatGPT Plus/Pro login) and calls the same backend the CLI uses directly:
//! `POST https://chatgpt.com/backend-api/codex/responses` (Responses API, SSE).
//!
//! ⚠ This is OpenAI's internal endpoint, reached with subscription credentials —
//! it can change without notice and is ToS-grey. It is gated behind the explicit
//! `chatgpt` provider so nothing uses it unless asked.

use crate::{Provider, Role, StreamItem, TurnRequest};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const DEFAULT_MODEL: &str = "gpt-5.5";
const CONTEXT_WINDOW: u64 = 272_000;
/// OAuth token endpoint + client id used by the codex CLI (same credentials we
/// reuse), so an expired access token can be refreshed in place instead of
/// dead-ending on "run codex login".
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Network retries for transient failures (5xx / 429 / connection errors).
const MAX_RETRIES: u32 = 2;

/// Best-known context window (tokens) for a ChatGPT-backend model, so
/// compaction adapts automatically per model instead of a fixed number.
fn model_context_window(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("gpt-5") || m.contains("gpt5") {
        400_000
    } else if m.contains("gpt-4.1") || m.contains("o3") || m.contains("o4") {
        1_000_000
    } else if m.contains("gpt-4") {
        128_000
    } else {
        CONTEXT_WINDOW
    }
}

/// Retry delay (ms) from `retry-after-ms` or `retry-after` (seconds) headers.
fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    let h = resp.headers();
    if let Some(ms) = h
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Some(ms);
    }
    h.get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s.saturating_mul(1000))
}

fn retry_delay_ms(resp: Option<&reqwest::Response>, attempt: u32) -> u64 {
    if let Some(delay) = resp.and_then(retry_after) {
        return delay.min(10_000);
    }
    let base = (500u64 << attempt.min(4)).min(10_000);
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_millis()) % (base / 5 + 1))
        .unwrap_or(0);
    base.saturating_add(jitter).min(10_000)
}

fn value_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

fn pct_u8(v: &Value) -> Option<u8> {
    value_f64(v).map(|n| n.round().clamp(0.0, 100.0) as u8)
}

fn unix_now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn reset_after_s(window: &Value) -> u64 {
    if let Some(s) = window["reset_after_seconds"]
        .as_u64()
        .or_else(|| window["resets_in_seconds"].as_u64())
        .or_else(|| window["reset_after"].as_u64())
    {
        return s;
    }
    let Some(mut at) = window["reset_at"]
        .as_u64()
        .or_else(|| window["reset_at_ms"].as_u64())
        .or_else(|| window["resetAtMs"].as_u64())
    else {
        return 0;
    };
    if at > 10_000_000_000 {
        at /= 1000;
    }
    at.saturating_sub(unix_now_s())
}

fn parse_usage_payload(v: &Value) -> Option<(String, u8, u8, u64, u64)> {
    let root = if v["usage"].is_object() {
        &v["usage"]
    } else {
        v
    };
    let rate = &root["rate_limit"];
    let primary = &rate["primary_window"];
    let secondary = &rate["secondary_window"];
    let p = pct_u8(&primary["used_percent"])?;
    let s = pct_u8(&secondary["used_percent"])?;
    let plan = root["plan_type"]
        .as_str()
        .or_else(|| root["plan"].as_str())
        .or_else(|| root["subscription_plan"].as_str())
        .unwrap_or("")
        .to_string();
    Some((plan, p, s, reset_after_s(primary), reset_after_s(secondary)))
}

fn parse_usage_headers(resp: &reqwest::Response) -> Option<(String, u8, u8, u64, u64)> {
    let h = resp.headers();
    let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    let pct = |k: &str| {
        hv(k)
            .and_then(|s| s.parse::<f64>().ok())
            .map(|n| n.round().clamp(0.0, 100.0) as u8)
    };
    let pct_from_limit = |limit_key: &str, remaining_key: &str| {
        let limit = hv(limit_key).and_then(|s| s.parse::<f64>().ok())?;
        let remaining = hv(remaining_key).and_then(|s| s.parse::<f64>().ok())?;
        if limit <= 0.0 {
            return None;
        }
        Some(
            (((limit - remaining).max(0.0) / limit) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8,
        )
    };
    let reset = |codex_key: &str, ratelimit_key: &str| {
        hv(codex_key)
            .or_else(|| hv(ratelimit_key))
            .and_then(parse_reset_header)
            .unwrap_or(0)
    };
    let p = pct("x-codex-primary-used-percent").or_else(|| {
        pct_from_limit(
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
        )
    })?;
    let s = pct("x-codex-secondary-used-percent")
        .or_else(|| pct_from_limit("x-ratelimit-limit-tokens", "x-ratelimit-remaining-tokens"))?;
    let plan = hv("x-codex-plan-type")
        .or_else(|| hv("x-codex-active-limit"))
        .unwrap_or("")
        .to_string();
    Some((
        plan,
        p,
        s,
        reset(
            "x-codex-primary-reset-after-seconds",
            "x-ratelimit-reset-requests",
        ),
        reset(
            "x-codex-secondary-reset-after-seconds",
            "x-ratelimit-reset-tokens",
        ),
    ))
}

fn parse_reset_header(raw: &str) -> Option<u64> {
    let mut n = raw.parse::<u64>().ok()?;
    if n > 10_000_000_000 {
        n /= 1000;
    }
    if n > 1_000_000_000 {
        Some(n.saturating_sub(unix_now_s()))
    } else {
        Some(n)
    }
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

fn account_id_from_claims(claims: &Value) -> Option<String> {
    for key in [
        "chatgpt_account_id",
        "https://api.openai.com/auth.chatgpt_account_id",
    ] {
        if let Some(id) = claims[key].as_str().filter(|s| !s.trim().is_empty()) {
            return Some(id.to_string());
        }
    }
    claims["organizations"].as_array()?.iter().find_map(|org| {
        org.as_str()
            .or_else(|| org["id"].as_str())
            .or_else(|| org["organization_id"].as_str())
            .filter(|s| !s.trim().is_empty())
            .map(ToString::to_string)
    })
}

fn account_id_from_access_token(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let decoded = base64url_decode(payload)?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    account_id_from_claims(&claims)
}

fn refresh_form(refresh: &str) -> [(&'static str, &str); 3] {
    [
        ("client_id", OAUTH_CLIENT_ID),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
    ]
}

fn error_summary(error: &Value, fallback: &str) -> String {
    let msg = error["message"].as_str().unwrap_or(fallback);
    let mut details = Vec::new();
    if let Some(code) = error["code"].as_str().filter(|s| !s.trim().is_empty()) {
        details.push(format!("code={code}"));
    }
    if let Some(kind) = error["type"].as_str().filter(|s| !s.trim().is_empty()) {
        details.push(format!("type={kind}"));
    }
    if details.is_empty() {
        msg.to_string()
    } else {
        format!("{msg} ({})", details.join(", "))
    }
}

async fn send_rate_limit(
    sink: &mpsc::Sender<StreamItem>,
    plan: String,
    primary_pct: u8,
    secondary_pct: u8,
    primary_reset_s: u64,
    secondary_reset_s: u64,
) -> bool {
    sink.send(StreamItem::RateLimit {
        plan,
        primary_pct,
        secondary_pct,
        primary_reset_s,
        secondary_reset_s,
    })
    .await
    .is_ok()
}

/// Poll the ChatGPT/Codex subscription usage in the background (owned args so
/// it can be spawned without borrowing `self`). Newer backends often omit
/// x-codex-* headers on the Responses stream; this is the reliable fallback.
async fn fetch_usage_snapshot(
    client: reqwest::Client,
    sink: mpsc::Sender<StreamItem>,
    access: String,
    account: String,
) {
    let mut req = client
        .get(USAGE_ENDPOINT)
        .bearer_auth(&access)
        .header("Accept", "application/json")
        .header("Origin", "https://chatgpt.com")
        .header("Referer", "https://chatgpt.com/codex");
    if !account.is_empty() {
        req = req.header("ChatGPT-Account-Id", &account);
    }
    let Ok(resp) = req.send().await else {
        return;
    };
    if !resp.status().is_success() {
        return;
    }
    let Ok(v) = resp.json::<Value>().await else {
        return;
    };
    let Some((plan, p, s, p_reset, s_reset)) = parse_usage_payload(&v) else {
        return;
    };
    let _ = send_rate_limit(&sink, plan, p, s, p_reset, s_reset).await;
}

pub struct ChatGptProvider {
    client: reqwest::Client,
    auth_path: String,
}

impl ChatGptProvider {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let auth_path = std::env::var("OXIDE_CODEX_AUTH")
            .unwrap_or_else(|_| format!("{home}/.codex/auth.json"));
        Self {
            client: crate::http_client(),
            auth_path,
        }
    }

    /// Read `(access_token, account_id, refresh_token)` from the codex auth file.
    /// `refresh_token` is empty if absent (then no in-place refresh is possible).
    fn credentials(&self) -> anyhow::Result<(String, String, String)> {
        let text = std::fs::read_to_string(&self.auth_path).map_err(|e| {
            anyhow::anyhow!(
                "ChatGPT subscription login not found ({}): {e}. Run `codex login` or open Codex Desktop and sign in again.",
                self.auth_path
            )
        })?;
        let v: Value = serde_json::from_str(&text)?;
        let refresh = v["tokens"]["refresh_token"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let at = v["tokens"]["access_token"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if at.trim().is_empty() && refresh.trim().is_empty() {
            anyhow::bail!(
                "ChatGPT subscription auth is missing access_token and refresh_token — run `codex login` to refresh it."
            );
        }
        let acc = v["tokens"]["account_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(ToString::to_string)
            .or_else(|| account_id_from_access_token(&at))
            .unwrap_or_default();
        Ok((at, acc, refresh))
    }

    /// Exchange a refresh token for a fresh `(access_token, refresh_token)` at the
    /// OAuth endpoint (same flow the codex CLI uses). The refresh token may rotate.
    async fn refresh_access(&self, refresh: &str) -> anyhow::Result<(String, String)> {
        let resp = self
            .client
            .post(OAUTH_TOKEN_URL)
            .form(&refresh_form(refresh))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("ChatGPT token refresh failed ({status}): {text}. Run `codex login` to sign in again.");
        }
        let v: Value = resp.json().await?;
        let access = v["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("token refresh response missing access_token"))?
            .to_string();
        // refresh_token may or may not rotate; keep the old one if not returned.
        let new_refresh = v["refresh_token"].as_str().unwrap_or(refresh).to_string();
        Ok((access, new_refresh))
    }

    /// Write refreshed tokens back to the codex auth file, preserving every other
    /// field (the file is shared with the codex CLI). Best-effort.
    fn persist_refreshed(&self, access: &str, refresh: &str) {
        let Ok(text) = std::fs::read_to_string(&self.auth_path) else {
            return;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(tokens) = v["tokens"].as_object_mut() {
            tokens.insert("access_token".into(), json!(access));
            tokens.insert("refresh_token".into(), json!(refresh));
        }
        // codex records the last refresh time; keep it roughly current.
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let secs = now.as_secs();
            v["last_refresh"] = json!(format!("{secs}"));
        }
        if let Ok(serialized) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(&self.auth_path, serialized);
        }
    }
}

impl Default for ChatGptProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// A UUID-shaped session id (format-valid; not cryptographically random).
fn session_id() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let h = format!("{n:032x}");
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

fn hash64(input: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in input.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn session_id_for(conversation_id: &str) -> String {
    let conv = conversation_id.trim();
    if conv.is_empty() {
        return session_id();
    }
    let h = format!(
        "{:016x}{:016x}",
        hash64(&format!("oxide-chatgpt-session:{conv}")),
        hash64(&format!("oxide-chatgpt-session-v2:{conv}"))
    );
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

/// Standard base64 (with padding) — small inline encoder so we don't pull in a
/// crate just to data-URL-encode attached images.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build the multimodal `content` for a user message: the text (with attachment
/// markers stripped) plus an `input_image` block per `wsimg:` marker (base64
/// data URL) — matching opencode/synara, so attached images reach the model
/// instead of the raw marker leaking as text.
fn user_content(text_with_markers: &str, cwd: &str) -> Value {
    let mut segs = text_with_markers.split('\u{2}');
    let mut text = segs.next().unwrap_or("").to_string();
    let mut images: Vec<Value> = Vec::new();
    for seg in segs {
        let Some(rel) = seg.strip_prefix("wsimg:") else {
            continue;
        };
        let path = if rel.starts_with('/') {
            std::path::PathBuf::from(rel)
        } else {
            std::path::Path::new(cwd).join(rel)
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mime = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("svg") => "image/svg+xml",
            _ => "image/png",
        };
        images.push(json!({
            "type": "input_image",
            "image_url": format!("data:{mime};base64,{}", base64_encode(&bytes)),
        }));
    }
    // The image is now actually sent — strip every "(user attached … NOT visible)"
    // note so it doesn't leak into the model's context. Loop to handle multiple
    // images (each adds its own parenthetical note).
    if !images.is_empty() {
        while let Some(i) = text.find("(user attached ") {
            let end = text[i..].find(')').map(|e| i + e + 1).unwrap_or(text.len());
            text.replace_range(i..end, "");
        }
    }
    let mut content = vec![json!({ "type": "input_text", "text": text.trim() })];
    content.extend(images);
    json!({ "type": "message", "role": "user", "content": content })
}

fn build_body(req: &TurnRequest) -> Value {
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&m.content);
            }
            // A tool result → a `function_call_output` paired by call_id, so the
            // model sees its call was satisfied (instead of an orphan user text).
            Role::Tool if m.tool_call_id.is_some() => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content,
            })),
            Role::User | Role::Tool => input.push(user_content(&m.content, &req.cwd)),
            Role::Assistant => {
                // Replay the model's own (encrypted) reasoning first so it keeps
                // its train of thought across rounds instead of re-thinking.
                if let Some(r) = &m.reasoning_item {
                    input.push(r.clone());
                }
                // Any assistant prose first, then a structured function_call item.
                if !m.content.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": m.content }]
                    }));
                }
                if let Some(tc) = &m.tool_call {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                    }));
                }
            }
        }
    }
    let model = if req.model.is_empty() {
        DEFAULT_MODEL
    } else {
        req.model.as_str()
    };
    let effort = if req.reasoning_effort.is_empty() {
        "medium"
    } else {
        req.reasoning_effort.as_str()
    };
    let mut body = json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "stream": true,
        "store": false,
        "temperature": req.temperature,
        "include": ["reasoning.encrypted_content"],
        // `summary: auto` streams reasoning summaries (shown live as thinking).
        "reasoning": { "effort": effort, "summary": "auto" }
    });
    // Stable cache key per conversation → backend prompt-cache hits across turns.
    if !req.conversation_id.trim().is_empty() {
        body["prompt_cache_key"] = json!(req.conversation_id);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect();
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    body
}

fn parse_tool_args(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

fn response_tool_call(
    item: &Value,
    // (name, args, call_id) — call_id seeded from output_item.added so the
    // drain path can use the real call_id even when output_item.done omits it.
    pending: &mut HashMap<String, (String, String, String)>,
) -> Option<(Vec<String>, String, Value)> {
    match item["type"].as_str()? {
        "function_call" => {
            let item_id = item["id"].as_str().unwrap_or("").to_string();
            let pending_item = if item_id.is_empty() {
                None
            } else {
                pending.remove(&item_id)
            };
            // Filter empty strings so we fall through to the pending fallback.
            let name = item["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    pending_item
                        .as_ref()
                        .map(|(n, _, _)| n.clone())
                        .filter(|n| !n.is_empty())
                })?;
            let raw = item["arguments"]
                .as_str()
                .map(str::to_string)
                .or_else(|| pending_item.as_ref().map(|(_, a, _)| a.clone()))
                .unwrap_or_else(|| "{}".to_string());
            // Prefer the call_id from the done event; fall back to the one
            // seeded at output_item.added time (stored in pending).
            let call_id = item["call_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    pending_item
                        .map(|(_, _, cid)| cid)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default();
            Some((vec![call_id, item_id], name, parse_tool_args(&raw)))
        }
        "shell_call" => {
            let commands: Vec<String> = item["action"]["commands"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|cmd| cmd.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if commands.is_empty() {
                return None;
            }
            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
            let item_id = item["id"].as_str().unwrap_or("").to_string();
            Some((
                vec![call_id, item_id],
                "shell".to_string(),
                json!({ "command": commands.join("\n") }),
            ))
        }
        _ => None,
    }
}

async fn send_tool_call(
    sink: &mpsc::Sender<StreamItem>,
    sent: &mut HashSet<String>,
    ids: Vec<String>,
    name: String,
    arguments: Value,
) -> bool {
    if ids.iter().any(|id| !id.is_empty() && sent.contains(id)) {
        return true;
    }
    let id = ids
        .iter()
        .find(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("{name}:{arguments}"));
    sent.insert(id.clone());
    for alias in ids {
        if !alias.is_empty() {
            sent.insert(alias);
        }
    }
    sink.send(StreamItem::ToolCall {
        id,
        name,
        arguments,
    })
    .await
    .is_ok()
}

async fn send_response_usage(
    sink: &mpsc::Sender<StreamItem>,
    response: &Value,
    model: &str,
) -> bool {
    let usage = &response["usage"];
    sink.send(StreamItem::Usage {
        input: usage["input_tokens"].as_u64().unwrap_or(0),
        output: usage["output_tokens"].as_u64().unwrap_or(0),
        context_window: Some(model_context_window(model)),
    })
    .await
    .is_ok()
}

#[async_trait]
impl Provider for ChatGptProvider {
    fn name(&self) -> &str {
        "chatgpt"
    }

    async fn stream(&self, req: TurnRequest, sink: mpsc::Sender<StreamItem>) -> anyhow::Result<()> {
        let (mut access, mut account, mut refresh) = self.credentials()?;
        let mut refreshed = false;
        if access.trim().is_empty() && !refresh.trim().is_empty() {
            let (new_access, new_refresh) = self.refresh_access(&refresh).await?;
            self.persist_refreshed(&new_access, &new_refresh);
            account = account
                .trim()
                .is_empty()
                .then(|| account_id_from_access_token(&new_access))
                .flatten()
                .unwrap_or(account);
            access = new_access;
            refresh = new_refresh;
            refreshed = true;
        }
        let chatgpt_session_id = session_id_for(&req.conversation_id);
        let body = build_body(&req);
        let active_model = if req.model.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            req.model.clone()
        };
        // POST with: a one-shot in-place token refresh on 401, and bounded
        // backoff retries on transient 429/5xx/connection errors.
        let mut attempt = 0u32;
        let resp = loop {
            let mut builder = self
                .client
                .post(ENDPOINT)
                .bearer_auth(&access)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("OpenAI-Beta", "responses=experimental")
                .header("originator", "codex_cli_rs")
                .header("session_id", &chatgpt_session_id)
                .json(&body);
            if !account.trim().is_empty() {
                builder = builder.header("chatgpt-account-id", &account);
            }
            let send_result = builder.send().await;
            match send_result {
                Ok(resp) if resp.status().is_success() => break resp,
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 429 {
                        if let Some((plan, p, s, p_reset, s_reset)) = parse_usage_headers(&resp) {
                            let _ = send_rate_limit(&sink, plan, p, s, p_reset, s_reset).await;
                        }
                    }
                    // Expired access token → refresh once in place, then retry.
                    if status.as_u16() == 401 && !refresh.is_empty() && !refreshed {
                        refreshed = true;
                        let (new_access, new_refresh) = self.refresh_access(&refresh).await?;
                        self.persist_refreshed(&new_access, &new_refresh);
                        if account.trim().is_empty() {
                            account = account_id_from_access_token(&new_access).unwrap_or_default();
                        }
                        access = new_access;
                        refresh = new_refresh;
                        continue;
                    }
                    // Transient → wait (honor retry-after) and retry.
                    if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES
                    {
                        let wait = retry_delay_ms(Some(&resp), attempt);
                        attempt += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                        continue;
                    }
                    let text = resp.text().await.unwrap_or_default();
                    if status.as_u16() == 401 {
                        anyhow::bail!("ChatGPT subscription token expired — run `codex login` or sign in again from Codex Desktop. ({text})");
                    }
                    if status.as_u16() == 403 {
                        anyhow::bail!("ChatGPT subscription rejected the request ({status}). Check that this account has Codex/ChatGPT subscription access and re-authenticate if needed. ({text})");
                    }
                    if status.as_u16() == 429 {
                        anyhow::bail!("ChatGPT subscription rate limit reached ({status}). Wait for the plan reset shown in Usage, then retry. ({text})");
                    }
                    if status.as_u16() == 413 || text.to_ascii_lowercase().contains("context") {
                        anyhow::bail!("ChatGPT subscription context is too large ({status}). Compact the chat or remove large attachments, then retry. ({text})");
                    }
                    anyhow::bail!("chatgpt {status}: {text}");
                }
                Err(_e) if attempt < MAX_RETRIES => {
                    let wait = retry_delay_ms(None, attempt);
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        };

        // Rate-limit snapshot: prefer headers on the stream response (free, no
        // extra RTT). If absent, spawn a background poll to /wham/usage so the
        // SSE loop starts immediately instead of waiting for a second HTTP round-trip
        // (which can add 1–2 s before the first token is visible to the user).
        if let Some((plan, p, s, p_reset, s_reset)) = parse_usage_headers(&resp) {
            let _ = send_rate_limit(&sink, plan, p, s, p_reset, s_reset).await;
        } else {
            tokio::spawn(fetch_usage_snapshot(
                self.client.clone(),
                sink.clone(),
                access.clone(),
                account.clone(),
            ));
        }

        let mut stream = resp.bytes_stream().eventsource();
        // (name, args, call_id) — all three seeded at output_item.added so the
        // drain path after the loop uses the real call_id even when output_item.done
        // arrives without one (or when the stream is cut before output_item.done fires).
        let mut pending_function_args: HashMap<String, (String, String, String)> = HashMap::new();
        let mut sent_tools: HashSet<String> = HashSet::new();
        // Track whether a terminal SSE event was received. If the loop ends via
        // None / connection error, we surface a truncation notice so the user
        // doesn't mistake a cut-short reply for a complete one.
        let mut stream_completed = false;
        while let Some(ev) = stream.next().await {
            let ev = match ev {
                Ok(e) => e,
                Err(_) => break, // connection error — fall through to truncation notice
            };
            // Skip empty events (SSE keep-alive blank lines) and the [DONE]
            // sentinel some endpoints append after response.completed.
            if ev.data.is_empty() {
                continue;
            }
            if ev.data.trim() == "[DONE]" {
                // Treat [DONE] as a clean terminal signal (same as response.completed).
                stream_completed = true;
                break;
            }
            let v: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue, // non-JSON line (comment, malformed) — skip
            };
            match v["type"].as_str() {
                Some("response.output_text.delta") => {
                    if let Some(t) = v["delta"].as_str() {
                        if sink
                            .send(StreamItem::TextDelta(t.to_string()))
                            .await
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                }
                Some("response.reasoning_summary_text.delta")
                | Some("response.reasoning_text.delta") => {
                    if let Some(t) = v["delta"].as_str() {
                        let _ = sink.send(StreamItem::ReasoningDelta(t.to_string())).await;
                    }
                }
                Some("response.output_item.added") => {
                    // Seed a function_call's buffer at the START so later argument
                    // deltas have a name to attach to (and the call_id is preserved for
                    // the drain path even if output_item.done arrives without call_id).
                    let item = &v["item"];
                    if item["type"].as_str() == Some("function_call") {
                        if let Some(item_id) = item["id"].as_str().filter(|s| !s.is_empty()) {
                            let name = item["name"].as_str().unwrap_or("").to_string();
                            let args = item["arguments"].as_str().unwrap_or("").to_string();
                            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
                            pending_function_args.entry(item_id.to_string()).or_insert((
                                name.clone(),
                                args.clone(),
                                call_id.clone(),
                            ));
                            if !args.is_empty() {
                                let _ = sink
                                    .send(StreamItem::ToolInputDelta {
                                        id: if call_id.is_empty() {
                                            item_id.to_string()
                                        } else {
                                            call_id.clone()
                                        },
                                        name,
                                        delta: args.clone(),
                                        accumulated: args,
                                    })
                                    .await;
                            }
                        }
                    }
                }
                Some("response.output_item.done") => {
                    let item = &v["item"];
                    if item["type"].as_str() == Some("reasoning") {
                        let _ = sink.send(StreamItem::ReasoningItem(item.clone())).await;
                    }
                    if let Some((ids, name, arguments)) =
                        response_tool_call(item, &mut pending_function_args)
                    {
                        if !send_tool_call(&sink, &mut sent_tools, ids, name, arguments).await {
                            return Ok(());
                        }
                    }
                }
                Some("response.function_call_arguments.delta") => {
                    // Accumulate streamed argument JSON so the call is complete even
                    // if the terminal done event omits the full arguments.
                    if let Some(item_id) = v["item_id"].as_str() {
                        if let Some(delta) = v["delta"].as_str() {
                            let entry = pending_function_args
                                .entry(item_id.to_string())
                                .or_insert((String::new(), String::new(), String::new()));
                            entry.1.push_str(delta);
                            let id = if entry.2.is_empty() {
                                item_id.to_string()
                            } else {
                                entry.2.clone()
                            };
                            let _ = sink
                                .send(StreamItem::ToolInputDelta {
                                    id,
                                    name: entry.0.clone(),
                                    delta: delta.to_string(),
                                    accumulated: entry.1.clone(),
                                })
                                .await;
                        }
                    }
                }
                Some("response.function_call_arguments.done") => {
                    let item_id = v["item_id"].as_str().unwrap_or("").to_string();
                    if item_id.is_empty() {
                        continue;
                    }
                    let entry = pending_function_args.entry(item_id.clone()).or_insert((
                        String::new(),
                        String::new(),
                        String::new(),
                    ));
                    if let Some(name) = v["name"].as_str().filter(|s| !s.is_empty()) {
                        entry.0 = name.to_string();
                    }
                    // Prefer authoritative final arguments; otherwise keep what the
                    // deltas accumulated.
                    if let Some(args) = v["arguments"].as_str().filter(|s| !s.is_empty()) {
                        entry.1 = args.to_string();
                        let id = if entry.2.is_empty() {
                            item_id.clone()
                        } else {
                            entry.2.clone()
                        };
                        let _ = sink
                            .send(StreamItem::ToolInputDelta {
                                id,
                                name: entry.0.clone(),
                                delta: String::new(),
                                accumulated: entry.1.clone(),
                            })
                            .await;
                    }
                }
                Some("response.completed") => {
                    let _ = send_response_usage(&sink, &v["response"], &active_model).await;
                    // Terminal event — stop reading. Don't keep the SSE loop alive
                    // waiting for the connection to close (that can hang the turn).
                    stream_completed = true;
                    break;
                }
                Some("response.failed") => {
                    let detail = error_summary(&v["response"]["error"], "response failed");
                    anyhow::bail!("ChatGPT response failed: {detail}");
                }
                Some("response.incomplete") => {
                    // A soft stop (length / content filter / etc.) — NOT an error.
                    // Surface a note and end the turn gracefully with whatever was
                    // produced, so a truncated response doesn't blow up the turn.
                    let _ = send_response_usage(&sink, &v["response"], &active_model).await;
                    let reason = v["response"]["incomplete_details"]["reason"]
                        .as_str()
                        .unwrap_or("unknown");
                    let _ = sink
                        .send(StreamItem::Notice(format!(
                            "⚠ response incomplete ({reason}) — compact context or retry with a smaller prompt."
                        )))
                        .await;
                    stream_completed = true;
                    break;
                }
                // Top-level error frame (not wrapped in a response object).
                Some("error") => {
                    let detail = if v["error"].is_object() {
                        error_summary(&v["error"], "stream error")
                    } else {
                        error_summary(&v, "stream error")
                    };
                    anyhow::bail!("ChatGPT stream error: {detail}");
                }
                _ => {}
            }
        }
        // If the loop ended without a terminal event, the connection dropped
        // mid-stream — the response is incomplete. Notify the user so they
        // don't mistake a truncated reply for a full one.
        if !stream_completed {
            let _ = sink
                .send(StreamItem::Notice(
                    "⚠ connection lost mid-stream — response may be truncated. Retry if needed."
                        .into(),
                ))
                .await;
        }
        for (item_id, (name, raw_args, call_id)) in pending_function_args {
            // Skip buffers that never received a tool name (seeded by an
            // output_item.added that never resolved) — they aren't real calls.
            if name.is_empty() {
                continue;
            }
            // Use the real call_id (seeded at output_item.added) so the replay
            // correctly pairs function_call / function_call_output by call_id.
            let ids = if call_id.is_empty() {
                vec![item_id]
            } else {
                vec![call_id, item_id]
            };
            if !send_tool_call(
                &sink,
                &mut sent_tools,
                ids,
                name,
                parse_tool_args(&raw_args),
            )
            .await
            {
                return Ok(());
            }
        }
        let _ = sink.send(StreamItem::Done).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_protocol::ToolSpec;

    fn req_with_tools(tools: Vec<ToolSpec>) -> TurnRequest {
        TurnRequest {
            model: "gpt-5.5".to_string(),
            reasoning_effort: "medium".to_string(),
            temperature: 0.2,
            messages: Vec::new(),
            tools,
            cwd: "/tmp".to_string(),
            conversation_id: "session".to_string(),
            cli_resume: None,
        }
    }

    #[test]
    fn body_uses_responses_function_tool_shape() {
        let req = req_with_tools(vec![ToolSpec::new("shell", "Run a shell command")
            .mutating(true)
            .params(json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }))]);

        let body = build_body(&req);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "shell");
        assert_eq!(tool["strict"], false);
        assert_eq!(tool["parameters"]["required"][0], "command");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn body_sets_cache_key_and_reasoning_summary() {
        let body = build_body(&req_with_tools(Vec::new()));
        assert_eq!(body["prompt_cache_key"], "session");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 0.0001);
        assert_eq!(body["store"], false);
    }

    fn jwt_with_payload(payload: Value) -> String {
        let encoded = base64_encode(payload.to_string().as_bytes())
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        format!("header.{encoded}.signature")
    }

    #[test]
    fn account_id_falls_back_to_jwt_claims() {
        let token = jwt_with_payload(json!({
            "chatgpt_account_id": "acct_primary",
            "organizations": ["acct_other"]
        }));

        assert_eq!(
            account_id_from_access_token(&token),
            Some("acct_primary".to_string())
        );
    }

    #[test]
    fn account_id_uses_namespaced_claim_or_organization() {
        let namespaced = jwt_with_payload(json!({
            "https://api.openai.com/auth.chatgpt_account_id": "acct_namespaced"
        }));
        let org = jwt_with_payload(json!({
            "organizations": [{ "id": "org_123" }]
        }));

        assert_eq!(
            account_id_from_access_token(&namespaced),
            Some("acct_namespaced".to_string())
        );
        assert_eq!(
            account_id_from_access_token(&org),
            Some("org_123".to_string())
        );
    }

    #[test]
    fn refresh_form_matches_oauth_contract() {
        assert_eq!(
            refresh_form("refresh_token_value"),
            [
                ("client_id", OAUTH_CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", "refresh_token_value"),
            ]
        );
    }

    #[test]
    fn error_summary_includes_code_and_type() {
        let detail = error_summary(
            &json!({
                "message": "too large",
                "code": "context_length_exceeded",
                "type": "invalid_request_error"
            }),
            "response failed",
        );

        assert!(detail.contains("too large"));
        assert!(detail.contains("context_length_exceeded"));
        assert!(detail.contains("invalid_request_error"));
    }

    #[test]
    fn parses_wham_usage_payload() {
        let payload = json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 29.4,
                    "reset_after_seconds": 3600,
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": "44.6",
                    "reset_at": unix_now_s() + 86_400,
                    "limit_window_seconds": 604800
                }
            }
        });

        let (plan, primary, secondary, primary_reset, secondary_reset) =
            parse_usage_payload(&payload).unwrap();

        assert_eq!(plan, "pro");
        assert_eq!(primary, 29);
        assert_eq!(secondary, 45);
        assert_eq!(primary_reset, 3600);
        assert!(secondary_reset <= 86_400);
        assert!(secondary_reset > 86_300);
    }

    #[test]
    fn parses_wrapped_usage_payload() {
        let payload = json!({
            "usage": {
                "plan_type": "plus",
                "rate_limit": {
                    "primary_window": { "used_percent": 6, "reset_after_seconds": 120 },
                    "secondary_window": { "used_percent": 24, "reset_after_seconds": 240 }
                }
            }
        });

        assert_eq!(
            parse_usage_payload(&payload),
            Some(("plus".into(), 6, 24, 120, 240))
        );
    }

    #[test]
    fn output_item_uses_pending_function_arguments_when_needed() {
        // call_id absent from the done event — must come from pending (seeded at added).
        let mut pending = HashMap::from([(
            "item_1".to_string(),
            (
                "todo_write".to_string(),
                r#"{"todos":[{"content":"Build","status":"in_progress"}]}"#.to_string(),
                "call_seeded".to_string(), // call_id seeded at output_item.added
            ),
        )]);
        let item = json!({
            "type": "function_call",
            "id": "item_1"
            // call_id deliberately absent: must fall back to pending's call_id
        });

        let (ids, name, args) = response_tool_call(&item, &mut pending).unwrap();

        assert_eq!(ids, vec!["call_seeded".to_string(), "item_1".to_string()]);
        assert_eq!(name, "todo_write");
        assert_eq!(args["todos"][0]["content"], "Build");
        assert!(pending.is_empty());
    }

    #[test]
    fn output_item_done_call_id_wins_over_pending() {
        // call_id present in the done event: that wins, pending's is ignored.
        let mut pending = HashMap::from([(
            "item_2".to_string(),
            (
                "shell".to_string(),
                r#"{"command":"ls"}"#.to_string(),
                "call_old".to_string(),
            ),
        )]);
        let item = json!({
            "type": "function_call",
            "id": "item_2",
            "call_id": "call_new",
            "name": "shell",
            "arguments": r#"{"command":"ls"}"#
        });

        let (ids, name, _args) = response_tool_call(&item, &mut pending).unwrap();

        assert_eq!(ids[0], "call_new");
        assert_eq!(name, "shell");
        assert!(pending.is_empty());
    }

    #[test]
    fn native_shell_call_maps_to_engine_shell_tool() {
        let mut pending = HashMap::new();
        let item = json!({
            "type": "shell_call",
            "id": "item_shell",
            "call_id": "call_shell",
            "action": { "commands": ["pwd", "ls -la"] }
        });

        let (ids, name, args) = response_tool_call(&item, &mut pending).unwrap();

        assert_eq!(
            ids,
            vec!["call_shell".to_string(), "item_shell".to_string()]
        );
        assert_eq!(name, "shell");
        assert_eq!(args["command"], "pwd\nls -la");
    }

    #[test]
    fn chatgpt_session_id_is_stable_per_conversation() {
        let a1 = session_id_for("oxide-session-a");
        let a2 = session_id_for("oxide-session-a");
        let b = session_id_for("oxide-session-b");

        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.len(), 36);
        assert_eq!(a1.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn base64_encodes_with_padding() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn user_content_emits_input_image_and_strips_note() {
        let img = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("target/tmp/oxide-usercontent-test.png");
        std::fs::create_dir_all(img.parent().unwrap()).unwrap();
        std::fs::write(&img, b"\x89PNG\r\n\x1a\n").unwrap();
        let text = format!(
            "Look at this (user attached 1 image — image content is NOT visible to you; ask the user to describe it if needed)\u{2}wsimg:{}",
            img.display()
        );
        let v = user_content(&text, "/tmp");
        let _ = std::fs::remove_file(&img);
        let content = v["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_text");
        let t = content[0]["text"].as_str().unwrap();
        assert!(t.contains("Look at this"));
        assert!(!t.contains("user attached")); // note stripped now image is sent
        assert_eq!(content[1]["type"], "input_image");
        assert!(content[1]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn user_content_plain_text_has_no_image_block() {
        let v = user_content("just text", "/tmp");
        let content = v["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "just text");
    }
}
