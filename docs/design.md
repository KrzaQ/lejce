# Design

Lejce runs coding agents for other programs. Two programs use it:

- **Kuterlapa** controls a Mac by voice and a game controller. It asks a
  model short questions and shows the model's work in an overlay.
- **Lemat** is a math board. It holds a long conversation with one agent
  and shows proofs step by step.

Both need the same things: start an engine, send it messages, read what it
does, answer its questions, and count tokens. Kuterlapa has this code now
(`core/src/engines.rs`, `activity.rs`, part of `approvals.rs`). Lejce takes
it over, so that both programs share one copy.

## Engines

| Engine      | Command                                                                          | Protocol                         |
| ----------- | -------------------------------------------------------------------------------- | -------------------------------- |
| Claude Code | `claude -p --input-format stream-json --output-format stream-json --verbose`     | JSON lines in, JSON lines out    |
| pi          | `pi --mode rpc`                                                                  | JSON-RPC style lines             |
| Codex       | `codex exec --json` (later)                                                      | JSON lines out                   |

Every engine reports the same events to the program:

- text and thinking, as they stream;
- a tool call, and later its result;
- a question for the user (a permission prompt, or a choice);
- token usage: input, cache read, cache write, output, cost;
- the end of a turn, or an error.

## Hosts

An engine runs on this machine or on another machine over SSH, in a given
folder (`Host` in `src/host.rs`). Over SSH, Lejce runs `ssh -T <destination>
'cd <folder> && exec <engine> ...'`. The engine then reads that machine's
files and its own settings there (`CLAUDE.md`, memory, hooks).

## Conversations

- One engine process serves one conversation and stays alive between
  messages, so the program does not pay the start-up time for each message.
- The program owns the conversation history, in its own format. The
  engine's own session files only help to resume: when they are gone, or
  when the user switches the engine, the program sends the history again.
- Lejce names the engine session explicitly (`--session-id`, `--resume`).
  It never uses `--continue`: that takes the latest conversation in the
  folder, which may belong to another program or to a terminal session.

## MCP servers

The program gives a list of MCP servers. Each engine takes them its own way:

- Claude Code: `--mcp-config` (stdio or HTTP servers);
- Codex: `-c mcp_servers.<name>...`;
- pi: no MCP support of its own, so an extension bridges the servers (as
  Kuterlapa's `pi-tools.ts` does now).

A server on this machine and an engine on another machine meet through SSH:
`ssh -R` forwards a port (or a Unix socket) of the remote machine to the
local server. HTTP servers need nothing else on the remote side.

## Questions from the engine

- Claude Code: `--permission-prompt-tool` sends permission prompts to an MCP
  tool. Lejce turns the call into a question event.
- pi: `extension_ui_request` (select, confirm) from extensions such as a
  permission guard.
- Codex: its approval requests.

The program shows the question and returns the answer through Lejce.

## Cancelling

Each engine runs in its own process group. Cancelling stops the whole group,
so that tools the engine started stop too.

## Steps

1. Move the code from Kuterlapa: process start and cancel, the stream
   parsers, usage, the pi dialogs. Kuterlapa then depends on Lejce.
2. Long-lived conversations and explicit session ids (Lemat needs them;
   Kuterlapa sends one message per process now).
3. SSH hosts with forwarded MCP servers.
4. Permission prompts for Claude Code through an MCP tool.
5. Codex.
