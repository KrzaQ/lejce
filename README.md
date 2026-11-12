# Lejce

Lejce ("reins" in Polish) is a Rust library that drives coding agents:
Claude Code, pi, and later Codex. It starts an engine on this machine or in
a folder on another machine over SSH, sends it messages, gives it MCP
servers, and turns its output into one stream of events.

[Kuterlapa](https://github.com/KrzaQ/kuterlapa) and
[Lemat](https://github.com/KrzaQ/lemat) use it.

Status: early. `docs/design.md` describes the plan.

## Build

```sh
make build
make test
```

`make help` lists all targets.

## License

MIT or Apache 2.0, at your choice.

## Credits

Written by krzaq together with Claude (Anthropic), through Claude Code.
