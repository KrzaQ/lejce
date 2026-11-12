//! MCP servers that a program gives the engine, and how they reach an
//! engine on another machine.
//!
//! A stdio server runs where the engine runs: over SSH, its command must
//! exist on the remote machine. An HTTP server on this machine reaches a
//! remote engine through a forwarded port (`ssh -R`).

use crate::host::Forward;
use serde_json::{json, Value};
use std::io::Read;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The engine starts the server and talks to it through pipes.
    Stdio { command: String, args: Vec<String> },
    /// The server runs already; the engine connects to its URL.
    Http { url: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    /// The name the engine knows the server by. Its tools are
    /// `mcp__<name>__<tool>`.
    pub name: String,
    pub transport: Transport,
    /// What the tools are for, for engines that show it to the model (pi).
    pub description: Option<String>,
    /// An HTTP server on this machine that a remote engine reaches through
    /// a forwarded port. On this machine nothing changes.
    pub forward: bool,
}

impl McpServer {
    /// A stdio server.
    pub fn stdio(name: &str, command: &str, args: Vec<String>) -> McpServer {
        McpServer {
            name: name.into(),
            transport: Transport::Stdio {
                command: command.into(),
                args,
            },
            description: None,
            forward: false,
        }
    }

    /// An HTTP server, forwarded to a remote engine.
    pub fn http(name: &str, url: &str) -> McpServer {
        McpServer {
            name: name.into(),
            transport: Transport::Http { url: url.into() },
            description: None,
            forward: true,
        }
    }

    /// The server's entry in an MCP configuration, the same for Claude
    /// Code's --mcp-config and pi's registerMcpServer.
    pub(crate) fn config(&self) -> Value {
        match &self.transport {
            Transport::Stdio { command, args } => json!({ "command": command, "args": args }),
            Transport::Http { url } => json!({ "type": "http", "url": url }),
        }
    }
}

/// The MCP servers as Claude Code's `--mcp-config` takes them: a JSON
/// object, `{"mcpServers": {"<name>": {...}}}`.
pub fn mcp_config(servers: &[McpServer]) -> Value {
    let servers: serde_json::Map<String, Value> = servers
        .iter()
        .map(|s| (s.name.clone(), s.config()))
        .collect();
    json!({ "mcpServers": servers })
}

/// The servers as a remote engine sees them, and the ports to forward.
/// Each forwarded server gets a random port on the remote machine's
/// loopback interface, and its URL points there. Fails for a forwarded
/// server that is not HTTP, or a URL Lejce cannot read.
pub(crate) fn forward(servers: &[McpServer]) -> Result<(Vec<McpServer>, Vec<Forward>), String> {
    let mut seen = Vec::new();
    let mut forwards = Vec::new();
    let mut remote = Vec::new();
    for server in servers {
        let mut server = server.clone();
        if server.forward {
            let Transport::Http { url } = &server.transport else {
                return Err(format!(
                    "MCP server {}: only an HTTP server can be forwarded",
                    server.name
                ));
            };
            let target = Url::parse(url)
                .ok_or_else(|| format!("MCP server {}: cannot read the URL {url}", server.name))?;
            let port = loop {
                let port = random_port();
                if !seen.contains(&port) {
                    seen.push(port);
                    break port;
                }
            };
            forwards.push(Forward {
                remote: port,
                host: target.host.clone(),
                port: target.port,
            });
            server.transport = Transport::Http {
                url: format!("{}://127.0.0.1:{port}{}", target.scheme, target.path),
            };
        }
        remote.push(server);
    }
    Ok((remote, forwards))
}

/// A port from 20000 to 59999. Lejce cannot ask the remote machine which
/// port is free before ssh starts; a taken one stops ssh with an error.
fn random_port() -> u16 {
    let mut bytes = [0u8; 2];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = random.read_exact(&mut bytes);
    }
    20000 + u16::from_le_bytes(bytes) % 40000
}

/// The parts of an HTTP URL that forwarding needs.
#[derive(Debug, PartialEq)]
struct Url {
    scheme: String,
    host: String,
    port: u16,
    /// The rest: the path and the query, with its leading "/".
    path: String,
}

impl Url {
    fn parse(url: &str) -> Option<Url> {
        let (scheme, rest) = url.split_once("://")?;
        let default = match scheme {
            "http" => 80,
            "https" => 443,
            _ => return None,
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        // An IPv6 address in brackets has colons of its own.
        let (host, port) = match authority.rfind(':') {
            Some(i) if !authority[i..].contains(']') => {
                (&authority[..i], authority[i + 1..].parse().ok()?)
            }
            _ => (authority, default),
        };
        if host.is_empty() || host.contains('@') {
            return None;
        }
        Some(Url {
            scheme: scheme.into(),
            host: host.into(),
            port,
            path: path.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = Url::parse("http://localhost:8080/mcp?x=1").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("localhost", 8080, "/mcp?x=1")
        );
        let u = Url::parse("http://[::1]:9000").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("[::1]", 9000, "")
        );
        assert_eq!(Url::parse("https://127.0.0.1/mcp").unwrap().port, 443);
        assert_eq!(Url::parse("ftp://host/"), None);
        assert_eq!(Url::parse("http://user@host/"), None);
    }

    #[test]
    fn forwarded_servers() {
        let servers = [
            McpServer::http("board", "http://localhost:8123/mcp"),
            McpServer::stdio("files", "files-server", vec![]),
        ];
        let (remote, forwards) = forward(&servers).unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(
            (forwards[0].host.as_str(), forwards[0].port),
            ("localhost", 8123)
        );
        let port = forwards[0].remote;
        assert!((20000..60000).contains(&port));
        assert_eq!(
            remote[0].transport,
            Transport::Http {
                url: format!("http://127.0.0.1:{port}/mcp")
            }
        );
        assert_eq!(remote[1], servers[1]);
        let mut stdio = McpServer::stdio("x", "y", vec![]);
        stdio.forward = true;
        assert!(forward(&[stdio]).is_err());
    }

    #[test]
    fn config_for_claude() {
        let servers = [
            McpServer::stdio("tools", "/bin/server", vec!["--only".into(), "a,b".into()]),
            McpServer::http("board", "http://127.0.0.1:9/mcp"),
        ];
        assert_eq!(
            mcp_config(&servers).to_string(),
            r#"{"mcpServers":{"board":{"type":"http","url":"http://127.0.0.1:9/mcp"},"tools":{"args":["--only","a,b"],"command":"/bin/server"}}}"#
        );
    }
}
