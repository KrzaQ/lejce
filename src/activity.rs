//! What a running request does: its thinking, its tool calls and the
//! tokens it has used. The engines write a stream of JSON events (`claude
//! --output-format stream-json`, `pi --mode rpc`). `Activity` reads them
//! line by line, so that a program can show the work while it happens.

use crate::usage::{number, Call};
use crate::Engine;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// How many characters of the thinking to keep: the latest part.
const THINKING: usize = 1200;

/// How many characters of a tool's result to keep.
const RESULT: usize = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Failed,
}

/// One tool call.
#[derive(Clone, Debug)]
pub struct Step {
    id: String,
    pub name: String,
    /// The arguments, short.
    pub detail: String,
    pub state: StepState,
    /// The start of the result, in one line.
    pub result: String,
}

/// One window of a plan's usage limit, as Claude reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    /// "5h" or "7d".
    pub name: &'static str,
    /// Used, from 0 to 1.
    pub used: f64,
    /// When the window starts again (Unix time).
    pub resets_at: i64,
}

impl Limit {
    fn seconds(&self) -> i64 {
        if self.name == "5h" {
            5 * 3600
        } else {
            7 * 24 * 3600
        }
    }

    /// Where the usage lands at the reset if it goes on at this pace, in
    /// percent (like the Claude Code status line). At most 999.
    pub fn pace(&self, now: i64) -> u64 {
        let elapsed = (now - (self.resets_at - self.seconds())).clamp(1, self.seconds());
        ((self.used * 100.0 * self.seconds() as f64 / elapsed as f64).round() as u64).min(999)
    }

    /// "1h20m", "12m", "3d4h": the time until the reset.
    pub fn left(&self, now: i64) -> String {
        let s = (self.resets_at - now).max(0);
        match s {
            0..=3599 => format!("{}m", s / 60),
            3600..=86_399 => format!("{}h{}m", s / 3600, s % 3600 / 60),
            _ => format!("{}d{}h", s / 86_400, s % 86_400 / 3600),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Activity {
    engine: Engine,
    /// Tool name prefixes to hide: those of the request's own MCP servers.
    /// A program knows its tools by their short names.
    prefixes: Vec<String>,
    pub started: Instant,
    /// How long the request took, once it has finished.
    pub finished: Option<Duration>,
    /// The latest thinking block: only its end, at most THINKING characters.
    pub thinking: String,
    pub steps: Vec<Step>,
    /// Tokens the model read, all its calls together (the context of each
    /// call, cached or not).
    pub input: u64,
    /// Tokens the model wrote: thinking, tool calls and text.
    output_done: u64,
    /// Characters written in the message that is still running. Its token
    /// count comes only at its end; until then, about 4 characters count as
    /// one token.
    output_chars: usize,
    /// Each model's use, for statistics.
    pub calls: Vec<Call>,
    /// How full the model's context window is: the tokens of its latest
    /// call (all it read, cached or not, and what it wrote). It differs
    /// from `input`, which adds up every call.
    pub context: u64,
    /// The size of that window, once the engine tells it.
    pub window: Option<u64>,
    /// Claude's plan limits: the five-hour and the seven-day window.
    pub limits: Vec<Limit>,
    /// The final answer and the error, when the engine has finished.
    pub answer: Option<String>,
    pub error: Option<String>,
    /// The engine has finished the message and waits for the next one.
    pub(crate) ended: bool,
}

impl Activity {
    /// The activity of a request to `engine`. Tools of the named MCP
    /// `servers` show without the server's prefix.
    pub fn new(engine: Engine, servers: &[&str]) -> Activity {
        let prefixes = servers
            .iter()
            .flat_map(|name| [format!("mcp__{name}__"), format!("{name}__")])
            .collect();
        Activity {
            engine,
            prefixes,
            started: Instant::now(),
            finished: None,
            thinking: String::new(),
            steps: Vec::new(),
            input: 0,
            output_done: 0,
            output_chars: 0,
            calls: Vec::new(),
            context: 0,
            window: None,
            limits: Vec::new(),
            answer: None,
            error: None,
            ended: false,
        }
    }

    pub fn engine(&self) -> Engine {
        self.engine
    }

    /// Tokens written so far, with an estimate for the running message.
    pub fn output(&self) -> u64 {
        self.output_done + (self.output_chars / 4) as u64
    }

    /// Reads one line of the engine's output. Returns true if something
    /// that a program shows changed.
    pub fn line(&mut self, line: &str) -> bool {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        match self.engine {
            Engine::Claude => self.claude(&event),
            Engine::Pi => self.pi(&event),
        }
    }

    fn think(&mut self, text: &str) {
        self.thinking.push_str(text);
        self.output_chars += text.len();
        if self.thinking.chars().count() > THINKING * 3 / 2 {
            self.thinking = tail(&self.thinking, THINKING);
        }
    }

    fn start_step(&mut self, id: &str, name: &str) {
        let name = self.prefixes.iter().fold(name, |n, prefix| {
            n.strip_prefix(prefix.as_str()).unwrap_or(n)
        });
        self.steps.push(Step {
            id: id.into(),
            name: name.into(),
            detail: String::new(),
            state: StepState::Running,
            result: String::new(),
        });
    }

    fn step(&mut self, id: &str) -> Option<&mut Step> {
        self.steps.iter_mut().rev().find(|s| s.id == id)
    }

    fn finish_step(&mut self, id: &str, failed: bool, result: &Value) {
        if let Some(step) = self.step(id) {
            step.state = if failed {
                StepState::Failed
            } else {
                StepState::Done
            };
            step.result = short(&result_text(result), RESULT);
        }
    }

    /// pi's own figure (`get_session_stats`), which it uses for compaction.
    pub fn set_context(&mut self, tokens: Option<u64>, window: Option<u64>) {
        if let Some(tokens) = tokens {
            self.context = tokens;
        }
        if window.is_some() {
            self.window = window;
        }
    }

    /// The work as JSON, to show it or to save it with the answer. The
    /// latest thinking goes with it: it shows later why the model did
    /// something.
    pub fn to_json(&self) -> Value {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|s| {
                let state = match s.state {
                    StepState::Running => "running",
                    StepState::Done => "done",
                    StepState::Failed => "failed",
                };
                json!({ "name": s.name, "detail": s.detail, "state": state, "result": s.result })
            })
            .collect();
        let mut value = json!({
            "input": self.input,
            "output": self.output(),
            "seconds": self.finished.unwrap_or_else(|| self.started.elapsed()).as_secs_f64(),
            "steps": steps,
            "context": self.context,
            "window": self.window,
        });
        if !self.limits.is_empty() {
            let now = now();
            value["limits"] = json!(self
                .limits
                .iter()
                .map(|l| json!({ "name": l.name, "used": l.used, "left": l.left(now), "pace": l.pace(now) }))
                .collect::<Vec<_>>());
        }
        if !self.thinking.trim().is_empty() {
            value["thinking"] = json!(self.thinking.trim());
        }
        value
    }

    /// `claude -p --output-format stream-json --verbose
    /// --include-partial-messages`.
    fn claude(&mut self, e: &Value) -> bool {
        match e["type"].as_str() {
            Some("stream_event") => {
                let event = &e["event"];
                match event["type"].as_str() {
                    Some("message_start") => {
                        let read = context(&event["message"]["usage"]);
                        self.input += read;
                        self.context = read;
                        self.output_chars = 0;
                        true
                    }
                    Some("content_block_start") => {
                        let block = &event["content_block"];
                        match block["type"].as_str() {
                            Some("thinking") => {
                                self.thinking.clear();
                                false
                            }
                            Some("tool_use") | Some("server_tool_use") => {
                                self.start_step(str(&block["id"]), str(&block["name"]));
                                true
                            }
                            _ => false,
                        }
                    }
                    Some("content_block_delta") => {
                        let delta = &event["delta"];
                        match delta["type"].as_str() {
                            Some("thinking_delta") => {
                                self.think(str(&delta["thinking"]));
                                true
                            }
                            Some("text_delta") => {
                                self.output_chars += str(&delta["text"]).len();
                                true
                            }
                            Some("input_json_delta") => {
                                self.output_chars += str(&delta["partial_json"]).len();
                                true
                            }
                            _ => false,
                        }
                    }
                    Some("message_delta") => {
                        let written = event["usage"]["output_tokens"].as_u64().unwrap_or(0);
                        self.output_done += written;
                        self.context += written;
                        self.output_chars = 0;
                        true
                    }
                    _ => false,
                }
            }
            // The plan's usage limits.
            Some("rate_limit_event") => {
                let windows = &e["rate_limit_info"]["unifiedWindows"];
                let limits: Vec<Limit> = [("5h", "five_hour"), ("7d", "seven_day")]
                    .iter()
                    .filter_map(|(name, key)| {
                        let w = &windows[*key];
                        Some(Limit {
                            name,
                            used: w["utilization"].as_f64()?,
                            resets_at: w["resetsAt"].as_i64()?,
                        })
                    })
                    .collect();
                if !limits.is_empty() {
                    self.limits = limits;
                }
                true
            }
            // The whole message: the tool calls with their arguments.
            Some("assistant") => {
                let mut changed = false;
                for block in e["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        let detail = arguments(&block["input"]);
                        if let Some(step) = self.step(str(&block["id"])) {
                            step.detail = detail;
                            changed = true;
                        }
                    }
                }
                changed
            }
            // Tool results.
            Some("user") => {
                let mut changed = false;
                for block in e["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        let failed = block["is_error"].as_bool().unwrap_or(false);
                        self.finish_step(str(&block["tool_use_id"]), failed, &block["content"]);
                        changed = true;
                    }
                }
                changed
            }
            Some("result") => {
                // The window of the model that did most of the reading: the
                // requested one, not one Claude Code used on the side.
                self.window = e["modelUsage"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .max_by_key(|(_, u)| {
                        number(&u["inputTokens"])
                            + number(&u["cacheReadInputTokens"])
                            + number(&u["cacheCreationInputTokens"])
                    })
                    .and_then(|(_, u)| u["contextWindow"].as_u64())
                    .or(self.window);
                for (model, u) in e["modelUsage"].as_object().into_iter().flatten() {
                    self.calls.push(Call::claude(model, u));
                }
                if e["is_error"].as_bool().unwrap_or(false) {
                    let errors: Vec<&str> = e["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let error = match e["result"].as_str() {
                        Some(result) => result.to_string(),
                        None if !errors.is_empty() => errors.join("; "),
                        None => e["subtype"].as_str().unwrap_or("error").to_string(),
                    };
                    self.error = Some(error);
                } else {
                    self.answer = Some(str(&e["result"]).trim().to_string());
                }
                // Claude Code waits for the next message now.
                self.ended = true;
                true
            }
            _ => false,
        }
    }

    /// The agent events of `pi --mode rpc` (the same as `pi --mode json`).
    fn pi(&mut self, e: &Value) -> bool {
        match e["type"].as_str() {
            Some("message_update") => {
                let event = &e["assistantMessageEvent"];
                match event["type"].as_str() {
                    Some("thinking_start") => {
                        self.thinking.clear();
                        false
                    }
                    Some("thinking_delta") => {
                        self.think(str(&event["delta"]));
                        true
                    }
                    Some("text_delta") | Some("toolcall_delta") => {
                        self.output_chars += str(&event["delta"]).len();
                        true
                    }
                    Some("toolcall_start") => {
                        self.start_step(str(&event["id"]), str(&event["toolName"]));
                        true
                    }
                    Some("toolcall_end") => {
                        let call = &event["toolCall"];
                        let detail = arguments(&call["arguments"]);
                        if let Some(step) = self.step(str(&call["id"])) {
                            step.detail = detail;
                        }
                        true
                    }
                    _ => false,
                }
            }
            Some("tool_execution_end") => {
                let failed = e["isError"].as_bool().unwrap_or(false);
                self.finish_step(str(&e["toolCallId"]), failed, &e["result"]["content"]);
                true
            }
            // pi-censor's classifier call, reported with pi.appendEntry.
            Some("entry_appended") if e["entry"]["customType"] == "pi-censor-usage" => {
                let data = &e["entry"]["data"];
                self.calls.push(Call::pi(
                    "classifier",
                    &data["provider"],
                    &data["model"],
                    &data["usage"],
                    &data["ms"],
                ));
                false
            }
            Some("message_end") if e["message"]["role"] == "assistant" => {
                let message = &e["message"];
                let usage = &message["usage"];
                self.calls.push(Call::pi(
                    "model",
                    &message["provider"],
                    &message["model"],
                    usage,
                    &message["durationMs"],
                ));
                let read = ["input", "cacheRead", "cacheWrite"]
                    .iter()
                    .map(|k| usage[k].as_u64().unwrap_or(0))
                    .sum::<u64>();
                self.input += read;
                self.output_done += usage["output"].as_u64().unwrap_or(0);
                if read > 0 {
                    self.context = read + usage["output"].as_u64().unwrap_or(0);
                }
                self.output_chars = 0;
                // pi tries again after some errors; the last message counts.
                if message["stopReason"] == "error" || message["stopReason"] == "aborted" {
                    self.error = Some(
                        message["errorMessage"]
                            .as_str()
                            .unwrap_or("error")
                            .to_string(),
                    );
                } else {
                    let text: Vec<&str> = message["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|b| b["type"] == "text")
                        .map(|b| str(&b["text"]))
                        .collect();
                    if !text.is_empty() {
                        self.answer = Some(text.join("").trim().to_string());
                    }
                    self.error = None;
                }
                true
            }
            _ => false,
        }
    }
}

/// The time now, in Unix seconds: what `Limit::pace` and `Limit::left`
/// take.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn str(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The tokens of a call's context: new, cached and written to the cache.
fn context(usage: &Value) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|k| usage[k].as_u64().unwrap_or(0))
    .sum()
}

/// A tool's arguments in one short line: the values, without the names
/// when there is one, `name: value` otherwise.
fn arguments(input: &Value) -> String {
    let Some(map) = input.as_object() else {
        return String::new();
    };
    let value = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let text = if map.len() == 1 {
        map.values().map(value).collect::<String>()
    } else {
        map.iter()
            .map(|(k, v)| format!("{k}: {}", value(v)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    short(&text, 70)
}

/// A tool result's text: a string, or the text blocks of a list.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// `text` in one line, at most `count` characters.
fn short(text: &str, count: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > count {
        format!("{}…", line.chars().take(count - 1).collect::<String>())
    } else {
        line
    }
}

/// The last `count` characters of `text`, from the start of a word.
pub fn tail(text: &str, count: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= count {
        return text.to_string();
    }
    let rest: String = chars[chars.len() - count..].iter().collect();
    match rest.find(char::is_whitespace) {
        Some(i) if i < 30 => rest[i..].trim_start().to_string(),
        _ => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_stream() {
        let lines = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":2,"cache_creation_input_tokens":6947,"cache_read_input_tokens":0}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"I should search."}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"mcp__tools__browser_tabs","input":{}}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"mcp__tools__browser_tabs","input":{"profile":"Work"}}]}}"#,
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":82}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"Window 1\n  Inbox"}]}]}}"#,
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":6947}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"12345678"}}}"#,
            r#"{"type":"result","is_error":false,"result":" 7 tabs. ","modelUsage":{"claude-opus-5-5":{"inputTokens":4,"outputTokens":96,"cacheReadInputTokens":6947,"cacheCreationInputTokens":7049,"cacheCreationEphemeral1hInputTokens":7049,"thinkingTokens":20,"webSearchRequests":0,"costUSD":0.08,"contextWindow":1000000}}}"#,
        ];
        let mut a = Activity::new(Engine::Claude, &["tools"]);
        for line in lines {
            a.line(line);
        }
        assert_eq!(a.thinking, "I should search.");
        assert_eq!(a.steps.len(), 1);
        assert_eq!(a.steps[0].name, "browser_tabs");
        assert_eq!(a.steps[0].detail, "Work");
        assert_eq!(a.steps[0].state, StepState::Done);
        assert_eq!(a.steps[0].result, "Window 1 Inbox");
        assert_eq!(a.input, 6949 + 7049);
        assert_eq!(a.output(), 82 + 2);
        assert_eq!(a.answer.as_deref(), Some("7 tabs."));
        assert!(a.ended);
        // The latest call: 2 new, 100 written to the cache, 6947 read from it.
        assert_eq!(a.context, 7049);
        assert_eq!(a.window, Some(1_000_000));
        assert_eq!(
            a.calls,
            vec![Call {
                role: "model",
                provider: "anthropic".into(),
                model: "claude-opus-5-5".into(),
                input: 4,
                cache_read: 6947,
                cache_write: 7049,
                cache_write_1h: Some(7049),
                output: 96,
                thinking: Some(20),
                web_searches: Some(0),
                cost: Some(0.08),
                ms: None,
            }]
        );
    }

    #[test]
    fn pi_stream_with_retry() {
        let lines = [
            r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Connection error.","usage":{"input":0,"output":0}}}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_start"}}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"The user asks"}}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","id":"c1","toolName":"mcp__tools__computer_apps"}}"#,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","toolCall":{"id":"c1","arguments":{}}}}"#,
            r#"{"type":"entry_appended","entry":{"type":"custom","customType":"pi-censor-usage","data":{"tool":"t","provider":"ollama","model":"qwen","usage":{"input":456,"output":177,"cacheRead":0,"cacheWrite":0,"reasoning":0,"cost":{"total":0}},"ms":2646,"stopReason":"stop"}}}"#,
            r#"{"type":"tool_execution_end","toolCallId":"c1","isError":true,"result":{"content":[{"type":"text","text":"no access"}]}}"#,
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking"},{"type":"text","text":"Safari."}],"stopReason":"stop","usage":{"input":519,"output":42,"cacheRead":0,"cacheWrite":0}}}"#,
        ];
        let mut a = Activity::new(Engine::Pi, &["tools"]);
        for line in lines {
            a.line(line);
        }
        assert_eq!(a.thinking, "The user asks");
        assert_eq!(a.steps[0].name, "computer_apps");
        assert_eq!(a.steps[0].state, StepState::Failed);
        assert_eq!(a.steps[0].result, "no access");
        assert_eq!((a.input, a.output()), (519, 42));
        assert_eq!(a.answer.as_deref(), Some("Safari."));
        assert_eq!(a.error, None);
        let roles: Vec<_> = a
            .calls
            .iter()
            .map(|c| (c.role, c.model.as_str(), c.input, c.ms))
            .collect();
        assert_eq!(
            roles,
            vec![
                ("model", "", 0, None),
                ("classifier", "qwen", 456, Some(2646)),
                ("model", "", 519, None)
            ]
        );
        assert_eq!(a.calls[1].thinking, None);
        assert_eq!(a.context, 519 + 42);
    }

    #[test]
    fn other_servers_keep_their_prefix() {
        let mut a = Activity::new(Engine::Pi, &["tools"]);
        a.line(r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","id":"c1","toolName":"mcp__other__read"}}"#);
        assert_eq!(a.steps[0].name, "mcp__other__read");
    }

    #[test]
    fn limits_and_pace() {
        let mut a = Activity::new(Engine::Claude, &[]);
        a.line(r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":20000},"seven_day":{"utilization":0.36,"resetsAt":604800}}}}"#);
        assert_eq!(a.limits.len(), 2);
        // Half of the five hours gone, 14% used: 28% at the reset.
        let five = &a.limits[0];
        assert_eq!(five.pace(20000 - 9000), 28);
        assert_eq!(five.left(20000 - 4800), "1h20m");
        assert_eq!(a.limits[1].left(0), "7d0h");
    }

    #[test]
    fn short_forms() {
        assert_eq!(
            arguments(&serde_json::json!({"a": 1, "b": "x  y"})),
            "a: 1, b: x y"
        );
        assert_eq!(tail("one two three four", 9), "four");
    }
}
