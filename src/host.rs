//! Where an engine runs: here, or in a folder on another machine over SSH.

use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// This machine, in the given folder (or the current one).
    Local { folder: Option<PathBuf> },
    /// Another machine. `destination` is what `ssh` takes: a name from
    /// ~/.ssh/config or user@host. `folder` is a path on that machine.
    Ssh {
        destination: String,
        folder: Option<String>,
    },
}

/// A port on the remote machine that leads to a port on this machine
/// (`ssh -R`), so that a remote engine reaches a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// The port on the remote machine's loopback interface.
    pub remote: u16,
    /// Where the connections go, as this machine sees it.
    pub host: String,
    pub port: u16,
}

/// The start of the line in which a remote shell reports the program's
/// process id.
pub const REMOTE_PID: &str = "lejce-pid";

impl Host {
    /// A command that runs `program` with `args` on this host.
    pub fn command(&self, program: &str, args: &[String]) -> Command {
        self.build(program, args, &[], &[], false)
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Host::Ssh { .. })
    }

    /// Like `command`, with environment variables for the program and,
    /// over SSH, port forwards. On this machine the program reaches local
    /// ports directly, so `forwards` do nothing there.
    ///
    /// Over SSH the remote shell reads one command line, so every word is
    /// quoted for it, and the variables go through `env`. `-T` because
    /// engines talk through pipes, not a terminal.
    ///
    /// Over SSH the remote shell first writes `lejce-pid <pid>` to the
    /// error output (`REMOTE_PID`). The program runs as that process, which
    /// leads its own process group there, so that `signal_group` can stop
    /// it and the tools it started: ending the local ssh leaves them
    /// running.
    pub fn launch(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        forwards: &[Forward],
    ) -> Command {
        self.build(program, args, env, forwards, true)
    }

    fn build(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        forwards: &[Forward],
        report_pid: bool,
    ) -> Command {
        match self {
            Host::Local { folder } => {
                let mut command = Command::new(program);
                command.args(args);
                command.envs(env.iter().map(|(k, v)| (k, v)));
                if let Some(folder) = folder {
                    command.current_dir(folder);
                }
                command
            }
            Host::Ssh {
                destination,
                folder,
            } => {
                let mut line = String::new();
                if report_pid {
                    line.push_str(&format!("printf '{REMOTE_PID} %s\\n' $$ >&2; "));
                }
                if let Some(folder) = folder {
                    line.push_str(&format!("cd {} && ", quote(folder)));
                }
                line.push_str("exec ");
                if !env.is_empty() {
                    line.push_str("env ");
                    for (name, value) in env {
                        line.push_str(&quote(&format!("{name}={value}")));
                        line.push(' ');
                    }
                }
                line.push_str(&quote(program));
                for arg in args {
                    line.push(' ');
                    line.push_str(&quote(arg));
                }
                let mut command = Command::new("ssh");
                command.arg("-T");
                if !forwards.is_empty() {
                    // A port that is taken on the remote side must stop
                    // the engine: without it, its tools would be missing.
                    command.args(["-o", "ExitOnForwardFailure=yes"]);
                }
                for f in forwards {
                    command
                        .arg("-R")
                        .arg(format!("{}:{}:{}", f.remote, f.host, f.port));
                }
                command.args(["--", destination, &line]);
                command
            }
        }
    }

    /// A command that sends `signal` ("TERM", "KILL") to the process group
    /// `pid` on this host. Over SSH it opens its own connection.
    pub fn signal_group(&self, pid: u32, signal: &str) -> Command {
        let words = [format!("-{signal}"), "--".into(), format!("-{pid}")];
        match self {
            Host::Local { .. } => {
                let mut command = Command::new("/bin/kill");
                command.args(words);
                command
            }
            Host::Ssh { destination, .. } => {
                let mut command = Command::new("ssh");
                let line = format!("kill {}", words.join(" "));
                command.args(["-T", "--", destination, &line]);
                command
            }
        }
    }
}

/// A word for a POSIX shell: kept as it is when it is safe, otherwise in
/// single quotes. A "~/" at the start stays outside the quotes, so the
/// remote shell expands it to the home folder.
fn quote(word: &str) -> String {
    if let Some(rest) = word.strip_prefix("~/") {
        return if rest.is_empty() {
            "~/".into()
        } else {
            format!("~/{}", quote(rest))
        };
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("plain-word_1.txt"), "plain-word_1.txt");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("~/studia/analiza"), "~/studia/analiza");
        assert_eq!(quote("~/a b"), "~/'a b'");
        assert_eq!(quote("zażółć"), "'zażółć'");
    }

    #[test]
    fn ssh_command_line() {
        let host = Host::Ssh {
            destination: "server".into(),
            folder: Some("~/notes".into()),
        };
        let command = host.command("claude", &["-p".into(), "hello world".into()]);
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            parts(&command),
            [
                "-T",
                "--",
                "server",
                "cd ~/notes && exec claude -p 'hello world'"
            ]
        );
    }

    #[test]
    fn local_command() {
        let host = Host::Local {
            folder: Some("/tmp".into()),
        };
        let command = host.command("pi", &["--mode".into(), "rpc".into()]);
        assert_eq!(command.get_program(), "pi");
        assert_eq!(parts(&command), ["--mode", "rpc"]);
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/tmp"))
        );
    }

    #[test]
    fn ssh_with_env_and_forward() {
        let host = Host::Ssh {
            destination: "server".into(),
            folder: None,
        };
        let env = [("SERVERS".to_string(), r#"{"a": 1}"#.to_string())];
        let forward = Forward {
            remote: 40000,
            host: "127.0.0.1".into(),
            port: 8080,
        };
        let command = host.launch("pi", &["-e".into(), "~/x.ts".into()], &env, &[forward]);
        assert_eq!(
            parts(&command),
            [
                "-T",
                "-o",
                "ExitOnForwardFailure=yes",
                "-R",
                "40000:127.0.0.1:8080",
                "--",
                "server",
                r#"printf 'lejce-pid %s\n' $$ >&2; exec env 'SERVERS={"a": 1}' pi -e ~/x.ts"#
            ]
        );
    }

    #[test]
    fn signal_a_remote_group() {
        let host = Host::Ssh {
            destination: "server".into(),
            folder: Some("~/notes".into()),
        };
        let command = host.signal_group(4321, "TERM");
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            parts(&command),
            ["-T", "--", "server", "kill -TERM -- -4321"]
        );
        let local = Host::Local { folder: None }.signal_group(4321, "KILL");
        assert_eq!(parts(&local), ["-KILL", "--", "-4321"]);
    }
}
