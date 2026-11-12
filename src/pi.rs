//! pi in its RPC mode (`pi --mode rpc`): each message goes in as a command,
//! and the input stays open, so that pi's extensions can ask the user
//! (pi-censor asks before a call it is unsure of) and the next message can
//! follow. pi has no MCP support of its own; a small extension (pi-mcp.ts)
//! registers the conversation's servers.

use crate::activity::Activity;
use crate::question::{Answer, Question};
use crate::run::{Session, Setup};
use crate::Host;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::process::{ChildStdin, Stdio};

/// Environment variables for the engine.
type Env = Vec<(String, String)>;

/// The extension that gives pi the MCP servers named in LEJCE_MCP_SERVERS.
const EXTENSION: &str = include_str!("pi-mcp.ts");

/// The arguments, and the environment the extension reads. The request
/// itself goes in later, as an RPC command (`send`).
pub(crate) fn args(setup: &Setup) -> io::Result<(Vec<String>, Env)> {
    let mut a: Vec<String> = ["--mode", "rpc", "--model", &setup.model]
        .map(String::from)
        .to_vec();
    match &setup.session {
        Session::None => a.push("--no-session".into()),
        // pi creates a missing session; the first message checks for that.
        Session::New(id) | Session::Resume(id) => a.extend(["--session-id".into(), id.clone()]),
    }
    let mut env = Vec::new();
    match &setup.tools {
        // The extension registers the servers for this run only; pi's own
        // settings stay as they are, so its extensions (a permission
        // classifier, for example) still judge each call.
        Some(tools) => {
            if !tools.servers.is_empty() {
                a.push("-e".into());
                a.push(extension(setup)?);
                env.push(("LEJCE_MCP_SERVERS".into(), servers(setup).to_string()));
            }
            let names: Vec<String> = tools
                .builtin
                .iter()
                .cloned()
                .chain(tools.servers.iter().map(|s| format!("mcp__{}__*", s.name)))
                .collect();
            a.extend(["--tools".into(), names.join(",")]);
        }
        None => a.push("--no-tools".into()),
    }
    if setup.project {
        a.extend(["--append-system-prompt".into(), setup.system.clone()]);
    } else {
        a.extend([
            "--no-context-files".into(),
            "--system-prompt".into(),
            setup.system.clone(),
        ]);
    }
    a.extend([
        "--thinking".into(),
        setup.effort.clone().unwrap_or_else(|| "off".into()),
    ]);
    Ok((a, env))
}

/// The servers as the extension reads them: `{"<name>": {<the server's
/// configuration>, "description": ...}}`.
fn servers(setup: &Setup) -> Value {
    let servers: serde_json::Map<String, Value> = setup
        .tools
        .iter()
        .flat_map(|t| &t.servers)
        .map(|s| {
            let mut config = s.config();
            config["description"] = json!(s
                .description
                .clone()
                .unwrap_or_else(|| format!("{} tools", s.name)));
            (s.name.clone(), config)
        })
        .collect();
    Value::Object(servers)
}

/// Where the extension lives on a remote machine.
const REMOTE_FOLDER: &str = "~/.cache/lejce";

/// Writes the extension where pi runs, if it is not there yet or changed:
/// to the program's folder here, or to REMOTE_FOLDER on a remote machine.
/// Returns its path, as pi on that machine sees it.
fn extension(setup: &Setup) -> io::Result<String> {
    if let Host::Ssh { destination, .. } = &setup.host {
        let path = format!("{REMOTE_FOLDER}/lejce-pi-mcp.ts");
        let script = format!("mkdir -p {REMOTE_FOLDER} && cat > {path}");
        let host = Host::Ssh {
            destination: destination.clone(),
            folder: None,
        };
        let mut copy = host
            .command("sh", &["-c".into(), script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        copy.stdin
            .take()
            .expect("piped stdin")
            .write_all(EXTENSION.as_bytes())?;
        if !copy.wait()?.success() {
            return Err(io::Error::other(format!("cannot copy it to {destination}")));
        }
        return Ok(path);
    }
    let path = setup.files.join("lejce-pi-mcp.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(EXTENSION) {
        std::fs::create_dir_all(&setup.files)?;
        std::fs::write(&path, EXTENSION)?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Sends one RPC command.
pub(crate) fn send(stdin: &mut ChildStdin, command: &Value) -> io::Result<()> {
    writeln!(stdin, "{command}")
}

/// Handles what only pi's RPC mode sends: answers to commands, the end of
/// the work, and questions from its extensions. Marks the activity ended
/// when pi waits for the next message. Returns true after a question, so
/// the program shows its work again.
pub(crate) fn record(
    line: &str,
    input: &mut Option<ChildStdin>,
    activity: &mut Activity,
    ask: &dyn Fn(&Question) -> Answer,
) -> bool {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    match record["type"].as_str() {
        // pi's own figure of how full the context is: the message is done.
        Some("response") if record["command"] == "get_session_stats" => {
            let usage = &record["data"]["contextUsage"];
            activity.set_context(usage["tokens"].as_u64(), usage["contextWindow"].as_u64());
            activity.ended = true;
            true
        }
        Some("response") if record["success"] == false => {
            activity.error = Some(
                record["error"]
                    .as_str()
                    .unwrap_or("pi refused the request")
                    .to_string(),
            );
            activity.ended = true;
            false
        }
        // The prompt did not start any work (a command, for example).
        Some("response")
            if record["command"] == "prompt" && record["data"]["disposition"] == "handled" =>
        {
            activity.ended = true;
            false
        }
        // pi will not go on by itself. Ask how full the context is; the
        // answer ends the message.
        Some("agent_settled") => {
            let asked = input.as_mut().map(|stdin| {
                send(
                    stdin,
                    &json!({ "id": "stats", "type": "get_session_stats" }),
                )
            });
            if !matches!(asked, Some(Ok(()))) {
                activity.ended = true;
            }
            false
        }
        Some("extension_ui_request") => {
            let Some(answer) = dialog(&record, ask) else {
                return false;
            };
            if let Some(stdin) = input.as_mut() {
                let _ = send(stdin, &answer);
            }
            true
        }
        _ => false,
    }
}

/// Asks the user an extension's question (select or confirm) and returns
/// pi's `extension_ui_response`. Other dialogs (text input, an editor) are
/// cancelled: a program that drives pi may have no way to type. Returns
/// None for a record that wants no answer (a notice, a status).
fn dialog(request: &Value, ask: &dyn Fn(&Question) -> Answer) -> Option<Value> {
    let id = request["id"].clone();
    let cancelled = json!({ "type": "extension_ui_response", "id": id, "cancelled": true });
    let method = request["method"].as_str().unwrap_or_default();
    if !matches!(method, "select" | "confirm" | "input" | "editor") {
        return None;
    }
    // A title like "pi-censor: why\n\nthe call": the first part is the
    // question, the rest its details.
    let title = request["title"].as_str().unwrap_or_default();
    let (text, rest) = title.split_once("\n\n").unwrap_or((title, ""));
    let detail = [rest, request["message"].as_str().unwrap_or_default()]
        .iter()
        .filter(|t| !t.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let mut question = Question {
        tool: None,
        text: text.into(),
        detail,
        options: Vec::new(),
        multiple: false,
    };
    match method {
        "select" => {
            let options: Vec<String> = request["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            if options.is_empty() {
                return Some(cancelled);
            }
            question.options = options;
            let chosen = match ask(&question) {
                Answer::Yes if question.options.len() == 1 => Some(0),
                Answer::Chose(picked) => picked.first().copied(),
                _ => None,
            };
            Some(match chosen.and_then(|i| question.options.get(i)) {
                Some(option) => {
                    json!({ "type": "extension_ui_response", "id": id, "value": option })
                }
                None => cancelled,
            })
        }
        "confirm" => {
            let yes = ask(&question) == Answer::Yes;
            Some(json!({ "type": "extension_ui_response", "id": id, "confirmed": yes }))
        }
        _ => {
            eprintln!("pi asked for text ({method}): cancelled");
            Some(cancelled)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpServer;
    use crate::run::Tools;
    use crate::{Engine, Host};
    use std::cell::RefCell;

    #[test]
    fn select_returns_the_option_itself() {
        let request = json!({ "type": "extension_ui_request", "id": "q1", "method": "select",
                              "title": "censor: unsure\n\nrm -rf build", "message": "in ~/project",
                              "options": ["Allow once", "Deny"] });
        let seen = RefCell::new(None);
        let answer = dialog(&request, &|q| {
            *seen.borrow_mut() = Some(q.clone());
            Answer::Chose(vec![1])
        });
        assert_eq!(
            answer,
            Some(json!({ "type": "extension_ui_response", "id": "q1", "value": "Deny" }))
        );
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.text, "censor: unsure");
        assert_eq!(seen.detail, "rm -rf build\nin ~/project");
        assert_eq!(seen.tool, None);
    }

    #[test]
    fn one_option_takes_yes() {
        let request = json!({ "id": 7, "method": "select", "title": "Go on?", "options": ["Go"] });
        let answer = dialog(&request, &|_| Answer::Yes);
        assert_eq!(answer.unwrap()["value"], "Go");
        let answer = dialog(&request, &|_| Answer::Unanswered);
        assert_eq!(answer.unwrap()["cancelled"], true);
    }

    #[test]
    fn confirm_text_and_notices() {
        let confirm = json!({ "id": 1, "method": "confirm", "title": "Sure?" });
        assert_eq!(
            dialog(&confirm, &|_| Answer::No).unwrap()["confirmed"],
            false
        );
        assert_eq!(
            dialog(&confirm, &|_| Answer::Yes).unwrap()["confirmed"],
            true
        );
        let input = json!({ "id": 2, "method": "input", "title": "Name?" });
        assert_eq!(
            dialog(&input, &|_| unreachable!()).unwrap()["cancelled"],
            true
        );
        let notice = json!({ "id": 3, "method": "notify", "message": "hi" });
        assert_eq!(dialog(&notice, &|_| unreachable!()), None);
    }

    #[test]
    fn tools_through_the_extension() {
        let files = std::env::temp_dir().join(format!("lejce-test-{}", std::process::id()));
        let setup = Setup {
            engine: Engine::Pi,
            host: Host::Local { folder: None },
            model: "local/model".into(),
            effort: None,
            system: "Be brief.".into(),
            project: true,
            session: Session::None,
            disable_hooks: false,
            tools: Some(Tools {
                builtin: vec!["bash".into()],
                servers: vec![McpServer::stdio("tools", "server", vec!["--x".into()])],
            }),
            files: files.clone(),
        };
        let (args, env) = args(&setup).unwrap();
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(at("--tools"), "bash,mcp__tools__*");
        assert_eq!(at("--append-system-prompt"), "Be brief.");
        assert_eq!(at("--thinking"), "off");
        assert_eq!(std::fs::read_to_string(at("-e")).unwrap(), EXTENSION);
        assert_eq!(
            env,
            [(
                "LEJCE_MCP_SERVERS".to_string(),
                r#"{"tools":{"args":["--x"],"command":"server","description":"tools tools"}}"#
                    .to_string()
            )]
        );
        let _ = std::fs::remove_dir_all(files);
    }
}
