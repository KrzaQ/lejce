//! The engines Lejce can run.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    /// Claude Code, `claude -p`.
    Claude,
    /// pi, `pi --mode rpc`.
    Pi,
}

impl Engine {
    /// The engine's name, as programs write it in their settings.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Claude => "claude",
            Engine::Pi => "pi",
        }
    }

    /// The engine with this name, if Lejce knows it.
    pub fn parse(name: &str) -> Option<Engine> {
        match name {
            "claude" => Some(Engine::Claude),
            "pi" => Some(Engine::Pi),
            _ => None,
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}
