//! Claude Code: `claude -p`, one request per process. The message goes in
//! as an argument; the events come out as stream-json.

use crate::run::{mcp_config, Request};

pub(crate) fn args(request: &Request) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |words: &[&str]| a.extend(words.iter().map(|w| w.to_string()));
    push(&["-p", "--model", &request.model, "--no-session-persistence"]);
    // Events while it works; the thinking as a summary (Claude Code leaves
    // it out by default).
    push(&[
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    push(&["--thinking-display", "summarized"]);
    match &request.tools {
        // Its own tools stay off except those named. Its permission
        // classifier ("auto") judges each tool call: nobody sits at a
        // terminal to answer Claude Code's own prompts.
        Some(tools) => {
            push(&["--tools", &tools.builtin.join(",")]);
            if !tools.servers.is_empty() {
                let config = mcp_config(&tools.servers).to_string();
                push(&["--mcp-config", &config, "--strict-mcp-config"]);
            }
            push(&["--permission-mode", "auto"]);
        }
        None => push(&["--tools", ""]),
    }
    let system = if request.project {
        "--append-system-prompt"
    } else {
        "--system-prompt"
    };
    push(&[system, &request.system]);
    if let Some(effort) = &request.effort {
        push(&["--effort", effort]);
    }
    push(&[&request.message]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{McpServer, Tools};
    use crate::{Engine, Host};

    fn request(tools: Option<Tools>) -> Request {
        Request {
            engine: Engine::Claude,
            host: Host::Local { folder: None },
            model: "sonnet".into(),
            effort: Some("low".into()),
            system: "Be brief.".into(),
            project: false,
            message: "Hello".into(),
            tools,
            files: "/tmp".into(),
        }
    }

    #[test]
    fn without_tools() {
        let args = args(&request(None)).join(" ");
        assert!(
            args.starts_with("-p --model sonnet --no-session-persistence"),
            "{args}"
        );
        assert!(
            args.ends_with("--tools  --system-prompt Be brief. --effort low Hello"),
            "{args}"
        );
    }

    #[test]
    fn with_a_server() {
        let tools = Tools {
            builtin: vec!["WebSearch".into(), "Bash".into()],
            servers: vec![McpServer {
                name: "tools".into(),
                command: "server".into(),
                args: vec![],
                description: None,
            }],
        };
        let args = args(&request(Some(tools)));
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(at("--tools"), "WebSearch,Bash");
        assert_eq!(
            at("--mcp-config"),
            r#"{"mcpServers":{"tools":{"args":[],"command":"server"}}}"#
        );
        assert_eq!(at("--permission-mode"), "auto");
    }
}
