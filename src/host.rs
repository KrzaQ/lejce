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

impl Host {
    /// A command that runs `program` with `args` on this host.
    ///
    /// Over SSH the remote shell reads one command line, so every word is
    /// quoted for it. `-T` because engines talk through pipes, not a
    /// terminal.
    pub fn command(&self, program: &str, args: &[String]) -> Command {
        match self {
            Host::Local { folder } => {
                let mut command = Command::new(program);
                command.args(args);
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
                if let Some(folder) = folder {
                    line.push_str(&format!("cd {} && ", quote(folder)));
                }
                line.push_str("exec ");
                line.push_str(&quote(program));
                for arg in args {
                    line.push(' ');
                    line.push_str(&quote(arg));
                }
                let mut command = Command::new("ssh");
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
}
