//! Codex: `codex app-server`, which speaks JSON-RPC on its input and
//! output. One process holds one thread; each message starts a turn in it.
//! Codex asks for approvals with requests of its own, which become
//! questions for the user.

use crate::activity::Activity;
use crate::mcp::{McpServer, Transport};
use crate::pi::send;
use crate::question::{Answer, Question};
use crate::run::{Permissions, Session, Setup};
use serde_json::{json, Value};
use std::process::ChildStdin;

/// The arguments: the app server, with the MCP servers as configuration
/// overrides (`-c mcp_servers.<name>.command=...`).
pub(crate) fn args(setup: &Setup) -> Vec<String> {
    let mut a = vec!["app-server".to_string()];
    for server in setup.tools.iter().flat_map(|t| &t.servers) {
        for (key, value) in config(server) {
            a.push("-c".into());
            a.push(format!(
                "mcp_servers.{}.{key}={value}",
                key_name(&server.name)
            ));
        }
    }
    a
}

/// A server's settings as TOML values. A JSON string or array of strings
/// is also valid TOML.
fn config(server: &McpServer) -> Vec<(&'static str, String)> {
    match &server.transport {
        Transport::Stdio { command, args } => vec![
            ("command", json!(command).to_string()),
            ("args", json!(args).to_string()),
        ],
        Transport::Http { url } => vec![("url", json!(url).to_string())],
    }
}

/// A TOML key: bare when it may be, otherwise quoted.
fn key_name(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        name.to_string()
    } else {
        json!(name).to_string()
    }
}

/// The first request: who the client is.
pub(crate) fn initialize() -> Value {
    json!({ "jsonrpc": "2.0", "id": "initialize", "method": "initialize", "params": {
        "clientInfo": { "name": "lejce", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": null,
    } })
}

/// The answer to `initialize` must be followed by this notification.
pub(crate) fn initialized() -> Value {
    json!({ "jsonrpc": "2.0", "method": "initialized" })
}

/// A new thread, or the one to resume. Codex names threads itself, so
/// `Session::New` takes Codex's id (`Conversation::session`).
pub(crate) fn thread(setup: &Setup) -> Value {
    // Without tools Codex keeps its shell, but may only read.
    let (sandbox, policy, reviewer) = match &setup.tools {
        None => ("read-only", "never", "user"),
        Some(tools) => {
            let sandbox = if tools.builtin.is_empty() {
                "read-only"
            } else {
                "workspace-write"
            };
            match tools.permissions {
                // The auto reviewer judges what would ask the user.
                Permissions::Auto => (sandbox, "on-request", "auto_review"),
                Permissions::Ask => (sandbox, "untrusted", "user"),
            }
        }
    };
    let mut params = json!({
        "approvalPolicy": policy,
        "approvalsReviewer": reviewer,
        "sandbox": sandbox,
    });
    if !setup.model.is_empty() {
        params["model"] = json!(setup.model);
    }
    // The folder's AGENTS.md stays with developer instructions; base
    // instructions replace Codex's own prompt.
    let key = if setup.project {
        "developerInstructions"
    } else {
        "baseInstructions"
    };
    params[key] = json!(setup.system);
    let method = match &setup.session {
        Session::None => {
            params["ephemeral"] = json!(true);
            "thread/start"
        }
        Session::New(_) => "thread/start",
        Session::Resume(id) => {
            params["threadId"] = json!(id);
            "thread/resume"
        }
    };
    json!({ "jsonrpc": "2.0", "id": "thread", "method": method, "params": params })
}

/// One message: a new turn in the thread.
pub(crate) fn turn(effort: Option<&str>, thread: &str, number: u64, message: &str) -> Value {
    let mut params = json!({
        "threadId": thread,
        "input": [{ "type": "text", "text": message, "text_elements": [] }],
        "summary": "auto",
    });
    if let Some(effort) = effort {
        params["effort"] = json!(effort);
    }
    json!({ "jsonrpc": "2.0", "id": format!("turn-{number}"), "method": "turn/start", "params": params })
}

/// Handles what is not a notification: Codex's requests (approvals) and
/// its answers to Lejce's requests. Returns true after a question, so the
/// program shows its work again.
pub(crate) fn record(
    line: &str,
    input: &mut Option<ChildStdin>,
    activity: &mut Activity,
    ask: &dyn Fn(&Question) -> Answer,
) -> bool {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if record.get("id").is_none() {
        return false;
    }
    if record.get("method").is_none() {
        // An answer to Lejce: only an error matters, it ends the turn.
        if let Some(error) = record.get("error") {
            activity.error = Some(error["message"].as_str().unwrap_or("error").to_string());
            activity.ended = true;
        }
        return false;
    }
    let (response, asked) = respond(&record, ask);
    if let Some(stdin) = input.as_mut() {
        if send(stdin, &response).is_err() {
            activity.ended = true;
        }
    }
    asked
}

/// The answer to one of Codex's requests, and whether the user was asked.
fn respond(record: &Value, ask: &dyn Fn(&Question) -> Answer) -> (Value, bool) {
    let id = record["id"].clone();
    let p = &record["params"];
    let text = |key: &str, default: &str| p[key].as_str().unwrap_or(default).to_string();
    let question = |tool: &str, text: String, detail: String| Question {
        tool: Some(tool.into()),
        text,
        detail,
        options: Vec::new(),
        multiple: false,
    };
    let yes = |q: &Question| ask(q) == Answer::Yes;
    let result = match record["method"].as_str().unwrap_or_default() {
        "item/commandExecution/requestApproval" => {
            let q = question(
                "shell",
                text("reason", "Run a command"),
                text("command", ""),
            );
            json!({ "decision": if yes(&q) { "accept" } else { "decline" } })
        }
        "item/fileChange/requestApproval" => {
            let q = question(
                "edit",
                text("reason", "Change files"),
                text("grantRoot", ""),
            );
            json!({ "decision": if yes(&q) { "accept" } else { "decline" } })
        }
        // The older forms of the same requests.
        "execCommandApproval" => {
            let command: Vec<&str> = p["command"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let q = question("shell", text("reason", "Run a command"), command.join(" "));
            json!({ "decision": if yes(&q) { json!("approved") } else { json!({ "denied": { "rejection": "The user said no." } }) } })
        }
        "applyPatchApproval" => {
            let files: Vec<&String> = p["fileChanges"]
                .as_object()
                .map(|m| m.keys().collect())
                .unwrap_or_default();
            let detail = files
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let q = question("edit", text("reason", "Change files"), detail);
            json!({ "decision": if yes(&q) { json!("approved") } else { json!({ "denied": { "rejection": "The user said no." } }) } })
        }
        method => {
            let error =
                json!({ "code": -32601, "message": format!("Lejce does not handle {method}") });
            return (json!({ "jsonrpc": "2.0", "id": id, "error": error }), false);
        }
    };
    (
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::Tools;
    use crate::{Engine, Host};

    fn setup(tools: Option<Tools>, session: Session) -> Setup {
        Setup {
            engine: Engine::Codex,
            host: Host::Local { folder: None },
            model: "gpt-test".into(),
            effort: Some("low".into()),
            system: "Be brief.".into(),
            project: false,
            tools,
            session,
            disable_hooks: false,
            files: "/tmp".into(),
        }
    }

    #[test]
    fn servers_as_config() {
        let tools = Tools {
            servers: vec![
                McpServer::stdio("tools", "/bin/server", vec!["--x".into()]),
                McpServer::http("my board", "http://127.0.0.1:9/mcp"),
            ],
            ..Default::default()
        };
        let a = args(&setup(Some(tools), Session::None));
        assert_eq!(
            a,
            [
                "app-server",
                "-c",
                r#"mcp_servers.tools.command="/bin/server""#,
                "-c",
                r#"mcp_servers.tools.args=["--x"]"#,
                "-c",
                r#"mcp_servers."my board".url="http://127.0.0.1:9/mcp""#,
            ]
        );
    }

    #[test]
    fn threads() {
        let none = thread(&setup(None, Session::None));
        assert_eq!(none["method"], "thread/start");
        assert_eq!(none["params"]["ephemeral"], true);
        assert_eq!(none["params"]["sandbox"], "read-only");
        assert_eq!(none["params"]["baseInstructions"], "Be brief.");
        let tools = Tools {
            builtin: vec!["shell".into()],
            permissions: Permissions::Ask,
            ..Default::default()
        };
        let resume = thread(&setup(Some(tools), Session::Resume("t1".into())));
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "t1");
        assert_eq!(resume["params"]["approvalPolicy"], "untrusted");
        assert_eq!(resume["params"]["sandbox"], "workspace-write");
        let t = turn(Some("low"), "t1", 2, "hi");
        assert_eq!(t["id"], "turn-2");
        assert_eq!(t["params"]["effort"], "low");
        assert_eq!(t["params"]["input"][0]["text"], "hi");
    }

    #[test]
    fn approvals() {
        let request = json!({ "jsonrpc": "2.0", "id": 0, "method": "item/commandExecution/requestApproval",
                              "params": { "command": "touch /tmp/x", "reason": null } });
        let seen = std::cell::RefCell::new(None);
        let (yes, asked) = respond(&request, &|q| {
            *seen.borrow_mut() = Some(q.clone());
            Answer::Yes
        });
        assert!(asked);
        assert_eq!(
            yes,
            json!({ "jsonrpc": "2.0", "id": 0, "result": { "decision": "accept" } })
        );
        let q = seen.into_inner().unwrap();
        assert_eq!(
            (q.tool.as_deref(), q.text.as_str(), q.detail.as_str()),
            (Some("shell"), "Run a command", "touch /tmp/x")
        );
        let (no, _) = respond(&request, &|_| Answer::Unanswered);
        assert_eq!(no["result"]["decision"], "decline");
        let other = json!({ "id": 5, "method": "item/tool/requestUserInput", "params": {} });
        let (error, asked) = respond(&other, &|_| unreachable!());
        assert!(!asked);
        assert_eq!(error["error"]["code"], -32601);
    }
}
