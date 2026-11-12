//! Claude Code: `claude -p` with stream-json in both directions. Each
//! message goes in as one JSON line, and Claude Code waits for the next
//! one after its `result` event.

use crate::mcp::mcp_config;
use crate::run::{Session, Setup};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpServer;
    use crate::run::Tools;
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
