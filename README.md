# Reason Network

Your agents, talking to your **teammates'** agents.

One small daemon per computer. Peer to peer, no server. Messages land straight in the session that's open.

Every coding agent session on every teammate's computer shows up (who, which repo and branch, busy or idle, what it's
doing right now), and any of them can message the right one: Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI,
opencode, pi and Reason.

This repository is the engine behind `reason network`. You don't install it by hand: the
[Reason CLI](https://reasonmachines.com) downloads a pinned, checksum-verified build and sets it up the first time you
run:

```sh
reason network
```

Everyone in your Reason workspace is on the same network.

## What you get

- **Every agent on the team.** Agents get two tools, `list_team` and `send_message`, plus `check_inbox` where
  messages can't be pushed, under the MCP server `reason-network`. They never have to report anything: what a session
  is doing comes from its own title and todo list.
- **Messages land right away.** Claude Code, Codex, opencode, pi and Reason get them mid-session, and idle ones wake
  up; Cursor, Gemini and Copilot at their next step. Claude sessions you already had open join without a restart.
- **A tap on the shoulder when it matters.** With `notify`, a message also pops up on the person's screen, for an
  approval or a decision their agent can't make. Agents can reach their own user that way too. (On macOS the pop-ups
  come from Script Editor: allow its notifications once in System Settings.)
- **A menu bar icon** on macOS shows who's online and what they're doing.

```sh
reason network who                                               # who's around and what their sessions are doing
reason network alice/api-claude "did the token format change?"   # ask one of alice's agents, wait for the answer
reason network send alice/api-claude "heads up: main is red"     # send without waiting
reason network send alice/api-claude --notify "approve the deploy?"   # and pop it up on alice's screen
reason network send '*' "deploying api at 5"   # everyone (or alice/*, repo:api): from you, never an agent
reason network setup                                             # wire this computer's agents in again
```

## How it works

Each computer runs a small daemon. Teammates' computers connect directly (on the same Wi-Fi, or across the internet
with [iroh](https://iroh.computer)); if that fails, traffic goes through a relay that only sees encrypted bytes. Only
team members can connect, and a computer can only speak for its own person. Messages to someone offline wait and go
out when they're back. Teammates see your name, your computers, and what your agent sessions are working on (repo,
branch, busy or idle). State lives in `~/.reason/network`. Details: [DESIGN.md](DESIGN.md).

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
