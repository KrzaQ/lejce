//! A conversation from the terminal: each line of input is one message.
//!
//!   cargo run --example talk -- <claude|pi|codex> <model> [options]
//!
//!   --ssh <destination>  run the engine on another machine
//!   --folder <path>      the engine's folder (its project)
//!   --new                save the conversation in a new engine session
//!   --resume <id>        continue an engine session
//!   --mcp <name>=<url>   an HTTP MCP server on this machine (repeatable;
//!                        forwarded to a remote engine)
//!   --no-hooks           turn off the user's Claude Code hooks
//!   --shell              give the engine its own shell tool
//!   --ask                ask the user before tool calls the engine's rules
//!                        do not settle (otherwise its classifier decides)
//!   --allow              answer the engine's questions with yes, or with
//!                        the first option (otherwise: no answer)
//!
//! It prints the session id first, so that a later run can resume it.

use lejce::{
    Answer, Conversation, Engine, Host, McpServer, Permissions, Question, Session, Setup, Tools,
};
use std::io::BufRead;

/// Prints the question; with --allow, takes yes or the first option.
fn ask(question: &Question, allow: bool) -> Answer {
    eprintln!(
        "[question: {} {:?} {}]",
        question.text, question.options, question.detail
    );
    match (allow, question.options.is_empty()) {
        (false, _) => Answer::Unanswered,
        (true, true) => Answer::Yes,
        (true, false) => Answer::Chose(vec![0]),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage =
        "usage: talk <claude|pi> <model> [--ssh dest] [--folder path] [--new | --resume id]";
    let (Some(engine), Some(model)) = (args.first().and_then(|e| Engine::parse(e)), args.get(1))
    else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let folder = value("--folder");
    let host = match value("--ssh") {
        Some(destination) => Host::Ssh {
            destination,
            folder,
        },
        None => Host::Local {
            folder: folder.map(Into::into),
        },
    };
    let session = match value("--resume") {
        Some(id) => Session::Resume(id),
        None if args.iter().any(|a| a == "--new") => Session::New(lejce::new_session_id()),
        None => Session::None,
    };
    let servers: Vec<McpServer> = args
        .windows(2)
        .filter(|w| w[0] == "--mcp")
        .filter_map(|w| w[1].split_once('='))
        .map(|(name, url)| McpServer::http(name, url))
        .collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let builtin: Vec<String> = match (flag("--shell"), engine) {
        (false, _) => Vec::new(),
        (true, Engine::Pi) => vec!["bash".into()],
        (true, Engine::Codex) => vec!["shell".into()],
        (true, _) => vec!["Bash".into()],
    };
    let permissions = if flag("--ask") {
        Permissions::Ask
    } else {
        Permissions::Auto
    };
    let setup = Setup {
        engine,
        host,
        model: model.clone(),
        effort: None,
        system: "You are a helpful assistant. Answer briefly.".into(),
        project: true,
        tools: (!servers.is_empty() || !builtin.is_empty()).then_some(Tools {
            builtin,
            servers,
            permissions,
        }),
        session,
        disable_hooks: args.iter().any(|a| a == "--no-hooks"),
        files: std::env::temp_dir().join("lejce-talk"),
    };
    let mut conversation = match Conversation::open(&setup) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("session: {}", conversation.session().unwrap_or("none"));
    let allow = args.iter().any(|a| a == "--allow");
    for line in std::io::stdin().lock().lines() {
        let Ok(message) = line else { break };
        let outcome = conversation.send(&message, &|_| {}, &|q| ask(q, allow));
        let a = &outcome.activity;
        let tools: Vec<&str> = a.steps.iter().map(|s| s.name.as_str()).collect();
        eprintln!(
            "[{} in, {} out, context {}, tools {:?}]",
            a.input,
            a.output(),
            a.context,
            tools
        );
        match outcome.answer {
            Ok(answer) => println!("{answer}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    conversation.close();
}
