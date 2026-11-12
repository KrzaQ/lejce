//! Running an engine: one process serves one conversation and stays alive
//! between messages, so that the program does not pay the start-up time
//! for each message. Each engine runs in its own process group, so that
//! cancelling stops the tools it started too.

use crate::activity::Activity;
use crate::mcp::{self, McpServer};
use crate::question::{Answer, Question};
use crate::{claude, pi, Engine, Forward, Host};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Lines, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{error, fmt};

/// The tools of a conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    /// The engine's own tools to keep, by the engine's names: "WebSearch"
    /// or "Bash" for Claude Code, "bash" for pi. Engines judge each call
    /// with their own permission rules.
    pub builtin: Vec<String>,
    /// MCP servers, with all their tools.
    pub servers: Vec<McpServer>,
}

/// Which engine session a conversation uses. The program keeps the
/// conversation's history itself; the engine's session only saves sending
/// it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Session {
    /// No session: the engine saves nothing.
    None,
    /// A new session with this id (`new_session_id`).
    New(String),
    /// The session with this id, from an earlier process. If the engine
    /// no longer has it, the conversation ends with `Error::SessionGone`.
    Resume(String),
}

/// How to start an engine for a conversation.
#[derive(Clone, Debug)]
pub struct Setup {
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
    /// None: no tools at all.
    pub tools: Option<Tools>,
    pub session: Session,
    /// Turn off the user's own hooks for this engine (Claude Code's
    /// `disableAllHooks`). Hooks that bind every session in a folder to
    /// something, a chat bot for example, should not catch the program's
    /// sessions. pi keeps its extensions: they guard the tools.
    pub disable_hooks: bool,
    /// A folder of the program's own where Lejce may write files that the
    /// engine reads (pi's extension for MCP servers).
    pub files: PathBuf,
}

/// The end of one message. The activity comes even when the message
/// failed: the tokens were used.
pub struct Outcome {
    pub activity: Activity,
    pub answer: Result<String, Error>,
}

#[derive(Debug)]
pub enum Error {
    /// The setup cannot work: a server that cannot be forwarded, say.
    Setup(String),
    /// Starting the engine, or talking to it, failed.
    Io { doing: String, error: io::Error },
    /// The program cancelled the conversation.
    Cancelled,
    /// The engine no longer has the session to resume. Start a new one and
    /// send the history again.
    SessionGone,
    /// The engine reported an error, or quit with one.
    Failed { engine: Engine, message: String },
    /// The engine ended without an answer. `stderr` is what it wrote there.
    NoAnswer { engine: Engine, stderr: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Setup(message) => f.write_str(message),
            Error::Io { doing, error } => write!(f, "{doing}: {error}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::SessionGone => write!(f, "the engine no longer has this session"),
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

/// An engine process, so that it can be cancelled.
struct Running {
    pid: u32,
    cancelled: bool,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

/// Stops every engine this program runs: the engines, their tool servers
/// and the tools they run, so nothing more happens after the user cancels.
/// Their conversations end with `Error::Cancelled`.
pub fn cancel_all() {
    let pids: Vec<u32> = RUNNING.lock().unwrap().iter().map(|r| r.pid).collect();
    pids.into_iter().for_each(cancel);
}

/// Stops one engine's process group: SIGTERM, and SIGKILL a second later
/// if any of it is still there.
fn cancel(pid: u32) {
    {
        let mut running = RUNNING.lock().unwrap();
        match running.iter_mut().find(|r| r.pid == pid) {
            Some(r) => r.cancelled = true,
            None => return,
        }
    }
    eprintln!("engine: cancelling process group {pid}");
    signal_group(pid, "TERM");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        signal_group(pid, "KILL");
    });
}

fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .args([format!("-{signal}"), "--".into(), format!("-{pid}")])
        .stderr(Stdio::null())
        .status();
}

fn cancelled(pid: u32) -> bool {
    RUNNING
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.pid == pid && r.cancelled)
}

/// Cancels one conversation from another thread.
#[derive(Clone, Copy, Debug)]
pub struct Canceller {
    pid: u32,
}

impl Canceller {
    /// Stops the engine and the tools it runs. The conversation's current
    /// message ends with `Error::Cancelled`, and the conversation with it;
    /// resume its session in a new one.
    pub fn cancel(&self) {
        cancel(self.pid);
    }
}

/// A new session id for `Session::New`: a random UUID, which every engine
/// takes.
pub fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = random.read_exact(&mut bytes);
    }
    // Version 4, variant 1 (RFC 9562).
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// One message on a new engine, then the engine quits: `Session::None`
/// for a request that needs no history.
///
/// `progress` gets what the engine does while it runs (thinking, tool
/// calls, tokens), at most ten times a second, and once more at the end.
/// `ask` asks the user a question from the engine (pi's extensions ask
/// this way) and waits for the answer. Both run on the calling thread.
pub fn run(
    setup: &Setup,
    message: &str,
    progress: &dyn Fn(&Activity),
    ask: &dyn Fn(&Question) -> Answer,
) -> Outcome {
    let mut conversation = match Conversation::open(setup) {
        Ok(c) => c,
        Err(error) => {
            return Outcome {
                activity: Activity::new(setup.engine, &[]),
                answer: Err(error),
            }
        }
    };
    let mut outcome = conversation.send(message, progress, ask);
    let engine = setup.engine;
    let (status, stderr, was_cancelled) = conversation.finish();
    outcome.answer = outcome.answer.and_then(|answer| {
        // A cancel that came after the answer still counts: the program
        // dropped the answer already.
        if was_cancelled {
            return Err(Error::Cancelled);
        }
        match status {
            Ok(s) if !s.success() => Err(Error::Failed {
                engine,
                message: stderr,
            }),
            _ => Ok(answer),
        }
    });
    outcome
}

/// A running engine with its conversation.
pub struct Conversation {
    engine: Engine,
    /// The MCP servers' names, whose tools show without their prefix.
    servers: Vec<String>,
    session: Option<String>,
    /// pi: check at the first message that the resumed session exists.
    check_resume: bool,
    child: Child,
    pid: u32,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    /// What the engine wrote to its error output.
    errors: Arc<Mutex<String>>,
    reader: Option<JoinHandle<()>>,
    messages: u64,
}

/// At most this much of the engine's error output is kept: the latest.
const ERRORS: usize = 16 * 1024;

/// The setup as the engine's machine sees it, and the ports to forward
/// there. On this machine it stays as it is.
fn remote(setup: &Setup) -> Result<(Setup, Vec<Forward>), Error> {
    let mut setup = setup.clone();
    let mut forwards = Vec::new();
    if setup.host.is_remote() {
        if let Some(tools) = setup.tools.as_mut() {
            let (servers, ports) = mcp::forward(&tools.servers).map_err(Error::Setup)?;
            tools.servers = servers;
            forwards = ports;
        }
    }
    Ok((setup, forwards))
}

impl Conversation {
    /// Starts the engine. It waits for the first message.
    pub fn open(setup: &Setup) -> Result<Conversation, Error> {
        let engine = setup.engine;
        let io = |doing: String| move |error| Error::Io { doing, error };
        // A remote engine reaches local servers through forwarded ports.
        let (setup, forwards) = &remote(setup)?;
        let (args, env) = match engine {
            Engine::Claude => (claude::args(setup), Vec::new()),
            Engine::Pi => pi::args(setup).map_err(io(format!("writing the {engine} extension")))?,
        };
        let mut command = setup.host.launch(engine.name(), &args, &env, forwards);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.process_group(0);
        let mut child = command.spawn().map_err(io(format!("starting {engine}")))?;
        let pid = child.id();
        RUNNING.lock().unwrap().push(Running {
            pid,
            cancelled: false,
        });
        let errors = Arc::new(Mutex::new(String::new()));
        let mut stderr = BufReader::new(child.stderr.take().expect("piped stderr"));
        let reader = {
            let errors = Arc::clone(&errors);
            std::thread::spawn(move || {
                let mut line = String::new();
                while matches!(stderr.read_line(&mut line), Ok(n) if n > 0) {
                    let mut text = errors.lock().unwrap();
                    text.push_str(&line);
                    if text.len() > ERRORS * 2 {
                        let mut cut = text.len() - ERRORS;
                        while !text.is_char_boundary(cut) {
                            cut += 1;
                        }
                        text.drain(..cut);
                    }
                    line.clear();
                }
            })
        };
        let session = match &setup.session {
            Session::None => None,
            Session::New(id) | Session::Resume(id) => Some(id.clone()),
        };
        Ok(Conversation {
            engine,
            servers: setup
                .tools
                .iter()
                .flat_map(|t| &t.servers)
                .map(|s| s.name.clone())
                .collect(),
            session,
            check_resume: engine == Engine::Pi && matches!(setup.session, Session::Resume(_)),
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().expect("piped stdout")).lines(),
            child,
            pid,
            errors,
            reader: Some(reader),
            messages: 0,
        })
    }

    /// The engine session's id, if it has one.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn canceller(&self) -> Canceller {
        Canceller { pid: self.pid }
    }

    /// Sends a message and waits for the answer. `progress` and `ask` work
    /// as in `run`.
    pub fn send(
        &mut self,
        message: &str,
        progress: &dyn Fn(&Activity),
        ask: &dyn Fn(&Question) -> Answer,
    ) -> Outcome {
        let servers: Vec<&str> = self.servers.iter().map(String::as_str).collect();
        let mut activity = Activity::new(self.engine, &servers);
        let answer = self.turn(message, &mut activity, progress, ask);
        Outcome { activity, answer }
    }

    fn turn(
        &mut self,
        message: &str,
        activity: &mut Activity,
        progress: &dyn Fn(&Activity),
        ask: &dyn Fn(&Question) -> Answer,
    ) -> Result<String, Error> {
        let engine = self.engine;
        if std::mem::take(&mut self.check_resume) && !self.pi_has_messages() {
            return Err(if cancelled(self.pid) {
                Error::Cancelled
            } else {
                Error::SessionGone
            });
        }
        self.messages += 1;
        let command = match engine {
            Engine::Claude => {
                json!({ "type": "user", "message": { "role": "user", "content": message } })
            }
            Engine::Pi => {
                json!({ "id": format!("prompt-{}", self.messages), "type": "prompt", "message": message })
            }
        };
        // A failed write means the engine has quit; its output says why.
        let _ = self.write(&command);
        let mut shown = Instant::now();
        let mut steps = 0;
        while !activity.ended {
            let Some(Ok(line)) = self.output.next() else {
                break;
            };
            if engine == Engine::Pi && pi::record(&line, &mut self.input, activity, ask) {
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
        activity.finished = Some(activity.started.elapsed());
        progress(activity);
        if !activity.ended {
            return Err(self.ended(activity).unwrap_or_else(|| Error::NoAnswer {
                engine,
                stderr: self.stderr(),
            }));
        }
        if cancelled(self.pid) {
            return Err(Error::Cancelled);
        }
        if let Some(message) = activity.error.clone() {
            if engine == Engine::Claude && message.starts_with("No conversation found") {
                return Err(Error::SessionGone);
            }
            return Err(Error::Failed { engine, message });
        }
        activity.answer.clone().ok_or_else(|| Error::NoAnswer {
            engine,
            stderr: self.stderr(),
        })
    }

    /// The error of an engine that stopped without ending the message:
    /// cancelled, or quit. None if it still runs.
    fn ended(&mut self, activity: &Activity) -> Option<Error> {
        if cancelled(self.pid) {
            return Some(Error::Cancelled);
        }
        self.input.take();
        let status = self.child.wait().ok()?;
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let stderr = self.stderr();
        Some(match &activity.error {
            Some(message) => Error::Failed {
                engine: self.engine,
                message: message.clone(),
            },
            None if !status.success() => Error::Failed {
                engine: self.engine,
                message: stderr,
            },
            None => Error::NoAnswer {
                engine: self.engine,
                stderr,
            },
        })
    }

    /// pi: whether the resumed session has messages. pi creates a missing
    /// session silently, and then the model would lack the history.
    fn pi_has_messages(&mut self) -> bool {
        if self
            .write(&json!({ "id": "state", "type": "get_state" }))
            .is_err()
        {
            return false;
        }
        for line in self.output.by_ref() {
            let Ok(line) = line else { break };
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if record["type"] == "response" && record["id"] == "state" {
                return record["data"]["messageCount"].as_u64().unwrap_or(0) > 0;
            }
        }
        false
    }

    fn write(&mut self, command: &Value) -> io::Result<()> {
        match self.input.as_mut() {
            Some(stdin) => pi::send(stdin, command),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn stderr(&self) -> String {
        self.errors.lock().unwrap().trim().to_string()
    }

    /// Ends the conversation: closes the engine's input, so that it quits,
    /// and waits for it.
    pub fn close(self) {
        let _ = self.finish();
    }

    /// The exit status, the error output, and whether it was cancelled.
    fn finish(mut self) -> (io::Result<ExitStatus>, String, bool) {
        self.input.take();
        let status = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        (status, self.stderr(), cancelled(self.pid))
    }
}

impl Drop for Conversation {
    /// A conversation that is dropped without `close` may be in the middle
    /// of a message: stop the engine, so that nothing runs on unseen.
    fn drop(&mut self) {
        if self.input.is_some() || self.reader.is_some() {
            self.input.take();
            if matches!(self.child.try_wait(), Ok(None)) {
                cancel(self.pid);
                let _ = self.child.wait();
            }
        }
        RUNNING.lock().unwrap().retain(|r| r.pid != self.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids() {
        let a = new_session_id();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert_ne!(a, new_session_id());
    }
}
