# company-comms

A team directory and message bus for coding agents. Every agent session on every teammate's machine (Claude
Code, Codex, Cursor, Gemini CLI, Copilot CLI, opencode) can see who is working on what right now, and message
the right session directly: across the LAN or the internet, peer to peer, with no server to run.

```
$ comms who
team vibes · you are alex on mbp
alex · mbp · this machine
  api-claude    claude-code  busy 2m   acme/api@feat/auth  push · ← you
                doing: moving sessions to JWT
  web-codex     codex        idle 14m  acme/web@main       push
alice · xps · direct 4ms
  api-cursor    cursor       busy 1m   acme/api@main       turn
                doing: fixing flaky refresh-token test
sam · nuc · offline 2h
push = sees messages right away, even when idle · turn = at its next step · pull = when it checks
```

Your agent can then just do `send_message(to="alice/api-cursor", message="I'm changing the token format in
auth/session.rs, heads up")`, and it lands in alice's Cursor session.

**Status:** early. Tested on macOS with real Claude Code and Codex sessions and with two machines; Linux builds
come from CI. Expect rough edges and tell us about them.

## Get started

One person sets up the team, once:

```sh
curl -fsSL https://asdf9384.github.io/company-comms/install.sh | sh
```

It asks your name and a team name, puts the coding agents it finds on comms, and gives you the **team link**
(also on your clipboard). Send it to your friends. The link opens a page with one command; they paste it into
a terminal, type their name, and they're in. The same link works for everyone and keeps working: send it in
the group chat and forget about it. `comms link` shows it again; `comms link --new` replaces it (removing
someone replaces it too).

Then restart agent sessions that were already running and ask one: "who's on the team?" Plain `comms` shows
who's around; `comms setup` changes which agents are on comms and how messages are handled.

There's no server and no host: every machine talks to the others directly, and any teammate's machine can let a
newcomer in, so someone just has to be online. Codex asks once to trust the new hooks: choose "Trust all and
continue". Gemini CLI only runs hooks and MCP servers in trusted folders.

Agents (or scripts) can do it all without prompts: `comms join <link> --as <name>`, `comms init <team>`.

On a Mac, comms sits in the menu bar while it runs: two speech bubbles, filled while a teammate is connected,
with the number of messages waiting for you. Its menu lists each machine's sessions and what they're doing,
and copies the team link. "Hide menu bar icon" turns it off (`comms setup` turns it back on).

## What agents get

| Tool | |
|---|---|
| `list_team` | Every live session: who, which machine, which agent, repo@branch, busy/idle, what it's doing |
| `send_message` | To a session, a person, a whole repo, or everyone (below) |
| `check_inbox` | Read messages, optionally wait for one (a reply, say) |
| `set_status` | The one line teammates see about what this session is doing; rename it |

| Address | Means |
|---|---|
| `alice/api-cursor` | one session (`api-cursor` alone works when the name is unique) |
| `alice/api-cursor@xps` | that session on a specific machine |
| `alice` | alice the person: their human inbox (`comms inbox`) |
| `alice/*` | all of alice's live sessions |
| `repo:api` | every live session working in a repo |
| `*` | everyone |

Sessions are named `<repo>-<agent>`. A new session in the same place takes over a dead one's name and its
unread mail, so "tell the api agent" keeps working across restarts.

## How messages reach each agent

| Agent | Speed | How |
|---|---|---|
| Claude Code | **push** | Injected into the conversation between tool calls; starts a turn if the session is idle |
| Codex | **push** | `codex queue` (runs after the current turn, or right away when idle); hooks as fallback |
| opencode | **push** | Plugin delivers into idle sessions within seconds |
| Cursor | turn | Hooks: shown after the next tool call, or as a follow-up when the turn ends |
| Gemini CLI | turn | Hooks: after the next tool call, or when the turn ends |
| Copilot CLI | turn | Hooks: after the next tool call, or when the turn ends |
| any other MCP client | pull | Seen when it calls `check_inbox`; every tool result mentions unread mail |

`send_message` tells the sender which of these applies, so agents know whether to wait for a reply.

## CLI

```sh
comms who [filter] [--all]        # the directory (filter by person, repo, agent, text)
comms send <to> <message>         # from you, the human; also `comms send alice "lunch?"`
comms inbox [--wait 60]           # your messages
comms team                        # people, machines, and whether each is direct or relayed
comms link [--new]                # the team link (or a new one; the old one stops working)
comms invite --me                 # a one-time four-word code to add another machine of yours
comms held / approve / reject     # messages waiting for your approval (see policies)
comms remove <person|machine>     # take someone out of the team
comms doctor                      # check that everything works
comms setup                       # choose which agents are on comms (--remove undoes everything)
comms leave                       # leave the team on this machine
```

## Trust and privacy

- **Only team machines can connect.** Membership is a list signed by members' machine keys and replicated to
  every machine; connections are authenticated by those keys (QUIC/TLS via [iroh](https://iroh.computer)).
  A machine can only send as its own person.
- **What teammates see:** session names, agent, repo and branch, busy/idle, the `doing` line, and the last
  component of the working directory. Set `share_paths = true` to show full paths.
- **Peer messages are framed as peer input,** never as instructions from the agent's own user. Agents are told
  to treat them like a colleague's request and to check with their user before anything destructive.
- **You decide what gets through** (`~/.comms/config.toml`):

  ```toml
  [inbound]
  default = "accept"        # or "hold" (you approve with `comms approve`) or "refuse"

  [inbound.people]
  sam = "hold"
  ```

  Each machine is also limited to 60 messages a minute.
- **The team link** carries the team's join secret and a few members' keys (after the `#`, which browsers never
  send to the server). Anyone holding it can join, so treat it like a group-chat invite; `comms link --new`
  replaces it. One-time four-word codes (`comms invite --code`) are for reading out loud or adding your own
  machines.
- **No server of ours.** Machines connect directly (on the LAN, or hole-punched across the internet). When
  that fails, traffic goes through a relay that only sees encrypted bytes: n0's public relays by default, or
  your own with `comms relay https://relay.example.com` (run [`iroh-relay`](https://docs.iroh.computer/concepts/relays)
  on any small server).

## Config

`~/.comms/config.toml`:

| Key | Default | |
|---|---|---|
| `person` | `$USER` | your name (set by `init`/`join`) |
| `share_paths` | `false` | show teammates full working directories |
| `share_unmanaged` | `false` | show teammates Claude Code sessions that don't have comms loaded |
| `tray` | `true` | macOS: show the menu bar icon while the background service runs |
| `lan` | `true` | find teammates on the local network (mDNS) |
| `inbound` | accept | see above |

## Troubleshooting

- **`comms doctor`** checks the daemon, the team connection (direct or relayed, per machine) and every agent.
- **An agent doesn't have the comms tools:** restart that session; agents load MCP servers at startup.
- **A teammate shows as offline:** their machine is asleep or comms isn't running there (`comms doctor` on
  their side). Messages wait in your outbox and go out when they're back.
- **macOS asks to let comms find devices on your local network:** allow it for direct LAN connections. If you
  don't, comms still works over the internet.
- **Codex sessions don't show up until they use a comms tool:** Codex hasn't been told to trust the hooks yet;
  run `/hooks` in Codex.
- Logs: `~/.comms/daemon.log`.

## Uninstall

```sh
comms setup --remove     # unwire every agent, remove the service (edited files keep a .bak-comms backup)
comms leave              # leave the team
rm -rf ~/.comms ~/.local/bin/comms
```

## How it works

One binary, `comms`: a daemon per machine (sessions, mailboxes, connections to teammates), an MCP server each
agent launches, hook handlers, and this CLI. See [DESIGN.md](DESIGN.md).

## Development

```sh
cargo test
cargo build && python3 tests/e2e/two_machines.py   # two machines over iroh, simulated agents (needs internet)
python3 tests/e2e/hooks.py                          # Cursor, Gemini, Copilot, opencode hook flows
# Two "machines" by hand:
COMMS_HOME=/tmp/a COMMS_MACHINE=laptop COMMS_NO_MDNS=1 cargo run -- init test --as alex --no-setup
COMMS_HOME=/tmp/a COMMS_MACHINE=laptop COMMS_NO_MDNS=1 cargo run -- invite
COMMS_HOME=/tmp/b COMMS_MACHINE=desktop COMMS_NO_MDNS=1 cargo run -- join comms1… --as alice --no-setup
```

Releases: push a `v*` tag; GitHub Actions builds macOS and Linux binaries and publishes them with `install.sh`.

## License

MIT OR Apache-2.0
