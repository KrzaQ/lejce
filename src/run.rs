//! Running one request: start the engine, read its events, return its
//! answer. Each engine runs in its own process group, so that cancelling
//! stops the tools it started too.

use crate::activity::Activity;
use crate::question::{Answer, Question};
use crate::{claude, pi, Engine, Host};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::{error, fmt};

/// An MCP server that the engine starts for a request (stdio).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    /// The name the engine knows the server by. Its tools are
    /// `mcp__<name>__<tool>`.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// What the tools are for, for engines that show it to the model (pi).
    pub description: Option<String>,
}

/// The tools of a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    /// The engine's own tools to keep, by the engine's names: "WebSearch"
    /// or "Bash" for Claude Code, "bash" for pi. Engines judge each call
    /// with their own permission rules.
    pub builtin: Vec<String>,
    /// MCP servers, with all their tools.
    pub servers: Vec<McpServer>,
}

/// One request: a system prompt, a message, an answer.
#[derive(Clone, Debug)]
pub struct Request {
    pub engine: Engine,
    /// Where the engine runs. Its folder is the project the engine sees.
    pub host: Host,
    /// The engine's model name.
    pub model: String,
    /// Claude Code's `--effort`, pi's `--thinking`.
    pub effort: Option<String>,
    pub system: String,
    /// True: keep the engine's own system prompt and the folder's
    /// CLAUDE.md or AGENTS.md, and add `system` to them. False: `system`
    /// replaces them, so no project changes the answer.
    pub project: bool,
    pub message: String,
    /// None: no tools at all.
    pub tools: Option<Tools>,
    /// A folder of the program's own where Lejce may write files that the
    /// engine reads (pi's extension for MCP servers).
    pub files: PathBuf,
}

/// The end of a request. The activity comes even when the request failed:
/// the tokens were used.
pub struct Outcome {
    pub activity: Activity,
    pub answer: Result<String, Error>,
}

#[derive(Debug)]
pub enum Error {
    /// Starting the engine, or talking to it, failed.
    Io { doing: String, error: io::Error },
    /// The program cancelled the request (`cancel_all`).
    Cancelled,
    /// The engine reported an error, or quit with one.
    Failed { engine: Engine, message: String },
    /// The engine ended without an answer. `stderr` is what it wrote there.
    NoAnswer { engine: Engine, stderr: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io { doing, error } => write!(f, "{doing}: {error}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Failed { engine, message } => write!(f, "{engine} failed: {message}"),
            Error::NoAnswer { engine, stderr } => write!(f, "{engine} gave no answer: {stderr}"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// A request that is running, so that it can be cancelled.
struct Running {
    pid: u32,
    cancelled: bool,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

/// Stops every request this program runs: the engines, their tool servers
/// and the tools they run, so nothing more happens after the user cancels.
/// Each process group gets SIGTERM, and SIGKILL a second later if any of
/// it is still there. The requests end with `Error::Cancelled`.
pub fn cancel_all() {
    let groups: Vec<u32> = {
        let mut running = RUNNING.lock().unwrap();
        running.iter_mut().for_each(|r| r.cancelled = true);
        running.iter().map(|r| r.pid).collect()
    };
    for pid in groups {
        eprintln!("engine: cancelling process group {pid}");
        signal_group(pid, "TERM");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            signal_group(pid, "KILL");
        });
    }
}

fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .args([format!("-{signal}"), "--".into(), format!("-{pid}")])
        .stderr(Stdio::null())
        .status();
}

/// Runs a request and waits for its answer.
///
/// `progress` gets what the engine does while it runs (thinking, tool
/// calls, tokens), at most ten times a second, and once more at the end.
/// `ask` asks the user a question from the engine (pi's extensions ask
/// this way) and waits for the answer. Both run on the calling thread.
pub fn run(
    request: &Request,
    progress: &dyn Fn(&Activity),
    ask: &dyn Fn(&Question) -> Answer,
) -> Outcome {
    let servers: Vec<&str> = request
        .tools
        .iter()
        .flat_map(|t| &t.servers)
        .map(|s| s.name.as_str())
        .collect();
    let mut activity = Activity::new(request.engine, &servers);
    let answer = drive(request, &mut activity, progress, ask);
    Outcome { activity, answer }
}

fn drive(
    request: &Request,
    activity: &mut Activity,
    progress: &dyn Fn(&Activity),
    ask: &dyn Fn(&Question) -> Answer,
) -> Result<String, Error> {
    let engine = request.engine;
    let io = |doing: String| move |error| Error::Io { doing, error };
    let (args, env) = match engine {
        Engine::Claude => (claude::args(request), Vec::new()),
        Engine::Pi => pi::args(request).map_err(io(format!("writing the {engine} extension")))?,
    };
    let mut command = request.host.command(engine.name(), &args);
    command.envs(env);
    // pi in RPC mode reads its commands from the input.
    let rpc = engine == Engine::Pi;
    command.stdin(if rpc { Stdio::piped() } else { Stdio::null() });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn().map_err(io(format!("starting {engine}")))?;
    let pid = child.id();
    RUNNING.lock().unwrap().push(Running {
        pid,
        cancelled: false,
    });
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let mut input = child.stdin.take();
    let sent = match input.as_mut() {
        Some(stdin) => pi::send(
            stdin,
            &json!({ "id": "prompt", "type": "prompt", "message": request.message }),
        ),
        None => Ok(()),
    };
    let mut shown = Instant::now();
    let mut steps = 0;
    if sent.is_ok() {
        for line in BufReader::new(child.stdout.take().expect("piped stdout")).lines() {
            let Ok(line) = line else { break };
            if rpc && pi::record(&line, &mut input, activity, ask) {
                progress(activity);
            }
            if activity.line(&line)
                && (shown.elapsed() >= Duration::from_millis(100) || activity.steps.len() != steps)
            {
                progress(activity);
                shown = Instant::now();
                steps = activity.steps.len();
            }
        }
    } else {
        drop(input.take());
        let _ = child.kill();
    }
    activity.finished = Some(activity.started.elapsed());
    progress(activity);
    let status = child.wait();
    let errors = errors.join().unwrap_or_default();
    let cancelled = {
        let mut running = RUNNING.lock().unwrap();
        let cancelled = running.iter().any(|r| r.pid == pid && r.cancelled);
        running.retain(|r| r.pid != pid);
        cancelled
    };
    if cancelled {
        return Err(Error::Cancelled);
    }
    sent.map_err(io(format!("sending the request to {engine}")))?;
    let status = status.map_err(io(format!("waiting for {engine}")))?;
    if let Some(message) = activity.error.clone() {
        return Err(Error::Failed { engine, message });
    }
    let stderr = errors.trim().to_string();
    if !status.success() {
        return Err(Error::Failed {
            engine,
            message: stderr,
        });
    }
    activity
        .answer
        .clone()
        .ok_or(Error::NoAnswer { engine, stderr })
}

/// The MCP servers as Claude Code's `--mcp-config` takes them: a JSON
/// object, `{"mcpServers": {"<name>": {"command": ..., "args": [...]}}}`.
pub fn mcp_config(servers: &[McpServer]) -> Value {
    let servers: serde_json::Map<String, Value> = servers
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                json!({ "command": s.command, "args": s.args }),
            )
        })
        .collect();
    json!({ "mcpServers": servers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_for_claude() {
        let server = McpServer {
            name: "tools".into(),
            command: "/bin/server".into(),
            args: vec!["mcp".into(), "--only".into(), "a,b".into()],
            description: Some("not for Claude".into()),
        };
        assert_eq!(
            mcp_config(&[server]).to_string(),
            r#"{"mcpServers":{"tools":{"args":["mcp","--only","a,b"],"command":"/bin/server"}}}"#
        );
    }
}
