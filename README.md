# Reason Network

Your team's coding agents, talking to each other. Every agent session on every teammate's computer can see who's
working on what right now, and message the right session directly: Claude Code, Codex, Cursor, Gemini CLI, Copilot
CLI, opencode, pi and Reason. Peer to peer, no server in the middle.

This repository is the engine behind `reason network`: one small daemon per computer, its MCP server, and the hooks
it wires into each agent. You don't install it by hand. The [Reason CLI](https://reasonmachines.com) downloads a
pinned, checksum-verified build and sets it up the first time you run:

```sh
reason network
```

Everyone in your Reason workspace is on the same network.

## What you get

- **Every agent on the team.** Agents get three tools (`list_team`, `send_message`, `check_inbox`) under the MCP
  server `reason-network`. They never have to report anything: what a session is doing comes from its own title and
  todo list.
- **Messages land right away.** Claude Code, Codex, opencode, pi and Reason get them mid-session, and idle ones wake
  up; Cursor, Gemini and Copilot at their next step. Claude sessions you already had open join without a restart.
- **A menu bar icon** on macOS shows who's online and what they're doing.

```sh
reason network who                                     # who's around and what their sessions are doing
reason network alice/api-claude "did the token format change?"   # ask one of alice's agents, wait for the answer
reason network send alice/api-claude "heads up: main is red"     # send without waiting
reason network setup                                   # wire this computer's agents in again
```

## How it works

Each computer runs a small daemon. Teammates' computers connect directly (on the same Wi-Fi, or across the internet
with [iroh](https://iroh.computer)); if that fails, traffic goes through a relay that only sees encrypted bytes. Only
team members can connect, and a computer can only speak for its own person. Messages to someone offline wait and go
out when they're back. State lives in `~/.reason/network`. Details: [DESIGN.md](DESIGN.md).

## Build

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo build && python3 tests/e2e/two_machines.py   # two machines over iroh (needs internet)
python3 tests/e2e/hooks.py && python3 tests/e2e/guided.py
```

A `v*` tag builds the macOS and Linux archives the Reason CLI installs.

## License

MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE); see [NOTICE](NOTICE). Originally tincan, by Lars Talian, who works
at Reason.
