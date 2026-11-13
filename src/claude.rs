//! Claude Code: `claude -p` with stream-json in both directions. Each
//! message goes in as one JSON line, and Claude Code waits for the next
//! one after its `result` event.

use crate::activity::Activity;
use crate::mcp::mcp_config;
use crate::pi::send;
use crate::question::{Answer, Question};
use crate::run::{Permissions, Session, Setup};
use serde_json::{json, Value};
use std::process::ChildStdin;

pub(crate) fn args(setup: &Setup) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |words: &[&str]| a.extend(words.iter().map(|w| w.to_string()));
    push(&["-p", "--model", &setup.model]);
    // Never --continue: it takes the latest conversation in the folder,
    // which may belong to another program or to a terminal session.
    match &setup.session {
        Session::None => push(&["--no-session-persistence"]),
        Session::New(id) => push(&["--session-id", id]),
        Session::Resume(id) => push(&["--resume", id]),
    }
    if setup.disable_hooks {
        push(&["--settings", r#"{"disableAllHooks":true}"#]);
    }
    // Events while it works; the thinking as a summary (Claude Code leaves
    // it out by default).
    push(&["--input-format", "stream-json"]);
    push(&[
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    push(&["--thinking-display", "summarized"]);
    match &setup.tools {
        // Its own tools stay off except those named.
        Some(tools) => {
            push(&["--tools", &tools.builtin.join(",")]);
            if !tools.servers.is_empty() {
                let config = mcp_config(&tools.servers).to_string();
                push(&["--mcp-config", &config, "--strict-mcp-config"]);
            }
            match tools.permissions {
                Permissions::Auto => push(&["--permission-mode", "auto"]),
                // The prompts come as control requests on the output;
                // `record` answers them.
                Permissions::Ask => push(&[
                    "--permission-mode",
                    "default",
                    "--permission-prompt-tool",
                    "stdio",
                ]),
            }
        }
        None => push(&["--tools", ""]),
    }
    let system = if setup.project {
        "--append-system-prompt"
    } else {
        "--system-prompt"
    };
    push(&[system, &setup.system]);
    if let Some(effort) = &setup.effort {
        push(&["--effort", effort]);
    }
    a
}

/// Answers Claude Code's control requests: a permission prompt
/// (`can_use_tool`) becomes a question for the user. Returns true after a
/// question, so the program shows its work again.
pub(crate) fn record(
    line: &str,
    input: &mut Option<ChildStdin>,
    activity: &mut Activity,
    ask: &dyn Fn(&Question) -> Answer,
) -> bool {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if record["type"] != "control_request" {
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

/// The control response to a control request, and whether the user was
/// asked.
fn respond(record: &Value, ask: &dyn Fn(&Question) -> Answer) -> (Value, bool) {
    let id = record["request_id"].clone();
    let request = &record["request"];
    if request["subtype"] != "can_use_tool" {
        let error = format!("Lejce does not handle {}", request["subtype"]);
        let response = json!({ "subtype": "error", "request_id": id, "error": error });
        return (
            json!({ "type": "control_response", "response": response }),
            false,
        );
    }
    let tool = request["tool_name"].as_str().unwrap_or("tool");
    let input = &request["input"];
    // The command or the path says more than the whole input.
    let detail = ["command", "file_path", "url", "pattern"]
        .iter()
        .find_map(|k| input[k].as_str())
        .map(String::from)
        .unwrap_or_else(|| input.to_string());
    let question = Question {
        tool: Some(tool.into()),
        text: request["description"].as_str().unwrap_or(tool).into(),
        detail,
        options: Vec::new(),
        multiple: false,
    };
    let decision = match ask(&question) {
        Answer::Yes => json!({ "behavior": "allow", "updatedInput": input }),
        Answer::Unanswered => json!({ "behavior": "deny", "message": "The user did not answer." }),
        _ => json!({ "behavior": "deny", "message": "The user said no." }),
    };
    let response = json!({ "subtype": "success", "request_id": id, "response": decision });
    (
        json!({ "type": "control_response", "response": response }),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpServer;
    use crate::run::Tools;

    #[test]
    fn permission_prompts() {
        let request = json!({ "type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "description": "Create a file",
            "input": { "command": "touch /tmp/x", "description": "Create a file" } } });
        let seen = std::cell::RefCell::new(None);
        let (yes, asked) = respond(&request, &|q| {
            *seen.borrow_mut() = Some(q.clone());
            Answer::Yes
        });
        assert!(asked);
        let q = seen.into_inner().unwrap();
        assert_eq!(
            (q.tool.as_deref(), q.text.as_str(), q.detail.as_str()),
            (Some("Bash"), "Create a file", "touch /tmp/x")
        );
        let r = &yes["response"];
        assert_eq!(r["subtype"], "success");
        assert_eq!(r["request_id"], "r1");
        assert_eq!(r["response"]["behavior"], "allow");
        assert_eq!(r["response"]["updatedInput"]["command"], "touch /tmp/x");
        let (no, _) = respond(&request, &|_| Answer::No);
        assert_eq!(no["response"]["response"]["behavior"], "deny");
        let other = json!({ "type": "control_request", "request_id": "r2",
                            "request": { "subtype": "hook_callback" } });
        let (error, asked) = respond(&other, &|_| unreachable!());
        assert!(!asked);
        assert_eq!(error["response"]["subtype"], "error");
    }

    #[test]
    fn ask_mode() {
        let tools = Tools {
            permissions: Permissions::Ask,
            ..Default::default()
        };
        let args = args(&setup(Some(tools), Session::None)).join(" ");
        assert!(
            args.contains("--permission-mode default --permission-prompt-tool stdio"),
            "{args}"
        );
    }
    use crate::{Engine, Host};

    fn setup(tools: Option<Tools>, session: Session) -> Setup {
        Setup {
            engine: Engine::Claude,
            host: Host::Local { folder: None },
            model: "sonnet".into(),
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
    fn without_tools() {
        let args = args(&setup(None, Session::None)).join(" ");
        assert!(
            args.starts_with(
                "-p --model sonnet --no-session-persistence --input-format stream-json"
            ),
            "{args}"
        );
        assert!(
            args.ends_with("--tools  --system-prompt Be brief. --effort low"),
            "{args}"
        );
    }

    #[test]
    fn sessions() {
        let new = args(&setup(None, Session::New("a1".into()))).join(" ");
        assert!(new.contains("--session-id a1"), "{new}");
        let old = args(&setup(None, Session::Resume("a1".into()))).join(" ");
        assert!(old.contains("--resume a1"), "{old}");
        assert!(
            !old.contains("--continue") && !old.contains("--no-session"),
            "{old}"
        );
    }

    #[test]
    fn hooks() {
        let mut s = setup(None, Session::None);
        assert!(!args(&s).contains(&"--settings".to_string()));
        s.disable_hooks = true;
        let a = args(&s);
        let i = a.iter().position(|w| w == "--settings").unwrap();
        assert_eq!(a[i + 1], r#"{"disableAllHooks":true}"#);
    }

    #[test]
    fn with_a_server() {
        let tools = Tools {
            builtin: vec!["WebSearch".into(), "Bash".into()],
            servers: vec![McpServer::stdio("tools", "server", vec![])],
            permissions: Permissions::Auto,
        };
        let args = args(&setup(Some(tools), Session::None));
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(at("--tools"), "WebSearch,Bash");
        assert_eq!(
            at("--mcp-config"),
            r#"{"mcpServers":{"tools":{"args":[],"command":"server"}}}"#
        );
        assert_eq!(at("--permission-mode"), "auto");
    }
}
