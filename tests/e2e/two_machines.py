"""Two 'machines' (two TINCAN_HOMEs) on one box, talking over iroh, with simulated agent sessions.

    cargo build && python3 tests/e2e/two_machines.py

Needs internet (iroh relays/discovery). Run inside a Claude Code session, the simulated Claude session pushes its
test messages into that real session (they say "ignore this").
"""
import json, os, re, subprocess, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "tincan")
D = tempfile.mkdtemp(prefix="tincan-e2e-")
H = {"alex": os.path.join(D, "alex"), "alice": os.path.join(D, "alice")}
MACHINE = {"alex": "laptop", "alice": "desktop"}
FAKE_CODEX = os.path.join(D, "fake-codex.sh")
QUEUE_LOG = os.path.join(D, "codex-queue.log")
# Stands in for osascript / notify-send, so the test shows nothing on screen: each machine logs what it would show.
FAKE_NOTIFY = os.path.join(D, "fake-notify.sh")
IN_CLAUDE = "CLAUDE_CODE_MESSAGING_SOCKET" in os.environ
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail}"))


def env(who, strip_claude=True):
    e = dict(os.environ, TINCAN_HOME=H[who], TINCAN_MACHINE=MACHINE[who], TINCAN_NO_MDNS="1", TINCAN_NO_SERVICE="1", TINCAN_CODEX_BIN=FAKE_CODEX)
    e.update({"TINCAN_OSASCRIPT_BIN": FAKE_NOTIFY, "TINCAN_NOTIFY-SEND_BIN": FAKE_NOTIFY, "DBUS_SESSION_BUS_ADDRESS": "test",
              "NOTIFY_LOG": os.path.join(D, f"notify-{who}.log")})
    if strip_claude:
        for k in list(e):
            if k.startswith("CLAUDE"):
                del e[k]
    # Never this machine's real Claude sessions: tincan could deliver into them.
    e["CLAUDE_CONFIG_DIR"] = os.path.join(D, "claude-config")
    return e


def cli(who, *args):
    r = subprocess.run([BIN, *args], env=env(who), capture_output=True, text=True, timeout=90)
    return (r.stdout + r.stderr).strip()


def hook(who, agent, event, payload):
    r = subprocess.run([BIN, "hook", agent, event], env=env(who), capture_output=True, text=True,
                       input=json.dumps(payload), timeout=30)
    return r.stdout.strip()


class Mcp:
    def __init__(self, who, client, strip_claude, cwd):
        self.p = subprocess.Popen([BIN, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env(who, strip_claude),
                                  cwd=cwd, text=True, bufsize=1)
        self.id = 0
        self.req("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": client, "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")

    def req(self, method, params):
        self.id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}) + "\n")
        while True:
            msg = json.loads(self.p.stdout.readline())
            if msg.get("id") == self.id:
                return msg

    def tool(self, name, args, meta=None):
        params = {"name": name, "arguments": args}
        if meta:
            params["_meta"] = meta
        res = self.req("tools/call", params).get("result", {})
        return "\n".join(c.get("text", "") for c in res.get("content", []))

    def close(self):
        self.p.stdin.close()
        self.p.wait(5)


with open(FAKE_CODEX, "w") as f:
    f.write(f'#!/bin/sh\nprintf "%s\\n" "$*" >> {QUEUE_LOG}\necho "Queued message"\n')
os.chmod(FAKE_CODEX, 0o755)
with open(FAKE_NOTIFY, "w") as f:
    f.write('#!/bin/sh\nprintf "%s\\n" "$*" >> "$NOTIFY_LOG"\n')
os.chmod(FAKE_NOTIFY, 0o755)


def shown(who):
    time.sleep(0.5)
    p = os.path.join(D, f"notify-{who}.log")
    return open(p).read() if os.path.exists(p) else ""
os.makedirs(os.path.join(D, "work"), exist_ok=True)

# --- team formation ---
out = cli("alex", "init", "vibes", "--as", "alex", "--no-setup")
check("init creates team", "Created team vibes. You are alex on laptop." in out, out)
out = cli("alex", "invite")
link = re.search(r"(https://\S+/join/#tincan1\S+)", out)
check("invite prints the team link", bool(link), out)
out = cli("alice", "join", link.group(1), "--as", "alice", "--no-setup")
check("join with the team link succeeds", "Joined team vibes as alice on desktop." in out, out)
check("joiner sees alex connected", re.search(r"alex\s+laptop\s+(direct|relay)", out) is not None, out)
out = cli("alex", "team")
check("founder sees alice connected", re.search(r"alice\s+desktop\s+(direct|relay)", out) is not None, out)
out = cli("alice", "join", link.group(1), "--as", "bob", "--no-setup")
check("joining a second team is refused while yours has others, and says how to switch",
      "already in team 'vibes' (with alex)" in out and "tincan leave" in out, out)
# Installed on its own and started a team by itself first: the team link still just works.
sam = dict(env("alice"), TINCAN_HOME=os.path.join(D, "sam"), TINCAN_MACHINE="mini")
r = subprocess.run([BIN, "init", "solo", "--as", "sam", "--no-setup"], env=sam, capture_output=True, text=True, timeout=90)
r = subprocess.run([BIN, "join", link.group(1), "--as", "sam", "--no-setup"], env=sam, capture_output=True, text=True, timeout=90)
check("a team of one is swapped for the team you join", "Joined team vibes as sam on mini." in r.stdout, r.stdout + r.stderr)
subprocess.run([BIN, "leave"], env=sam, capture_output=True, timeout=60)
subprocess.run([BIN, "stop"], env=sam, capture_output=True)
os.makedirs(os.path.join(D, "bob"), exist_ok=True)
bob = dict(env("alice"), TINCAN_HOME=os.path.join(D, "bob"), TINCAN_MACHINE="nuc")
out = cli("alex", "invite", "--code")
code = re.search(r"Invite code for a teammate: ([a-z]+(?:-[a-z]+){3})", out)
check("invite --code prints a four-word code", bool(code), out)
r = subprocess.run([BIN, "join", code.group(1).replace("-", " ").upper(), "--as", "bob", "--no-setup"], env=bob,
                   capture_output=True, text=True, timeout=90)
check("joining with just the code works", "Joined team vibes as bob on nuc." in r.stdout, r.stdout + r.stderr)
r = subprocess.run([BIN, "join", code.group(1), "--as", "bob2", "--no-setup"], env=dict(bob, TINCAN_HOME=os.path.join(D, "bob2")),
                   capture_output=True, text=True, timeout=90)
check("a used code doesn't work twice", "Joined" not in r.stdout + r.stderr, r.stdout + r.stderr)
subprocess.run([BIN, "stop"], env=dict(bob, TINCAN_HOME=os.path.join(D, "bob2")), capture_output=True)
out = subprocess.run([BIN, "link"], env=bob, capture_output=True, text=True, timeout=30).stdout
check("any member can show the same team link", link.group(1).split("#")[1][:40] in out.replace("\n", ""), out)
subprocess.run([BIN, "stop"], env=bob, capture_output=True)

# --- sessions on both machines ---
claude = Mcp("alex", "claude-code", strip_claude=False, cwd=ROOT)
codex = Mcp("alice", "codex-mcp-client", strip_claude=True, cwd=os.path.join(D, "work"))
meta = {"sessionId": "11111111-aaaa", "threadId": "11111111-aaaa"}
time.sleep(7)  # Claude's session registers after 3s; presence propagates every 2s
out = codex.tool("list_team", {}, meta)
claude_name = os.path.basename(ROOT).lower() + "-claude"
check("alice's agent sees alex's session across machines", claude_name in out and "alex · laptop" in out, out)
# (After Codex's first tool call registered it.) Nobody reports a status: tincan reads the plan step in progress from Codex's own transcript.
rollout = os.path.join(D, "rollout.jsonl")
plan = {"plan": [{"step": "set up fixtures", "status": "completed"}, {"step": "testing cross-machine tincan", "status": "in_progress"}]}
with open(rollout, "w") as f:
    f.write(json.dumps({"type": "response_item", "payload": {"type": "function_call", "name": "update_plan", "arguments": json.dumps(plan)}}) + "\n")
hook("alice", "codex", "doing", {"session_id": "11111111-aaaa", "cwd": os.path.join(D, "work"), "transcript_path": rollout})
time.sleep(3)
out = cli("alex", "who")
check("alex sees what alice's codex is doing, read from its plan", "work-codex" in out and "testing cross-machine tincan" in out, out)

# --- messages across machines ---
out = codex.tool("send_message", {"to": f"alex/{claude_name}", "message": "tincan e2e (cross-machine push): ignore this."}, meta)
want = "delivered into the session" if IN_CLAUDE else "only sees it when it checks"
check("cross-machine send reaches the Claude session", want in out, out)
out = cli("alex", "send", "alice/work-codex", "hello from alex's terminal")
check("a terminal send to a remote pull session is queued there", "only sees it when it checks its inbox" in out, out)
out = codex.tool("check_inbox", {}, meta)
check("remote message arrives in codex inbox, from alex/terminal", 'from="alex/terminal"' in out and "hello from alex's terminal" in out, out)
out = codex.tool("check_inbox", {}, meta)
check("…and is gone once read", out.startswith("No messages."), out)
out = cli("alice", "send", "alex", "dinner at 8?")
check("a person is not an address: the error lists their agent sessions",
      "messages go to agent sessions; alex's are:" in out and f"alex/{claude_name}" in out, out)
out = codex.tool("send_message", {"to": f"alex/{claude_name}", "message": "can you approve the deploy? (e2e test, ignore)", "notify": True}, meta)
check("notify: the message also pops up on the person's screen", "popped up for alex (laptop)" in out, out)
log = shown("alex")
check("…naming who asks, which session, and the ask", f"alice/work-codex → {claude_name}" in log and "approve the deploy" in log, log)
out = cli("alice", "send", f"alex/{claude_name}", "--notify", "and from the terminal (e2e test, ignore)")
check("one pop-up per person a minute", "not popped up: laptop had one from alice" in out and "from the terminal" not in shown("alex"), out)
out = codex.tool("send_message", {"to": "alex", "message": "x", "notify": True}, meta)
check("a teammate's pop-up goes with a message to one of their sessions", "messages go to agent sessions" in out, out)
out = codex.tool("send_message", {"to": "alice", "message": "need your ok (e2e test, ignore)", "notify": True}, meta)
check("an agent can tap its own user on the shoulder", "popped up for alice (desktop)" in out and "need your ok" in shown("alice"), out)
out = codex.tool("send_message", {"to": "alex/*", "message": "x", "notify": True}, meta)
check("notify doesn't go to groups", "not a group" in out, out)
# No waiting for presence: the terminal that just messaged alice can be answered at once.
out = codex.tool("send_message", {"to": "alex/terminal", "message": "codex here: replying to your terminal"}, meta)
check("an agent can reply at once to a terminal that just messaged it", "→ alex/terminal: queued" in out or "→ alex/terminal: delivered" in out, out)
time.sleep(1)
out = cli("alex", "listen", "--as", "terminal", "--wait", "5")
check("…and `tincan listen --as terminal` shows the reply", "replying to your terminal" in out and 'from="alice/work-codex"' in out, out)
cli("alice", "listen", "--as", "fresh", "--wait", "0")
cli("alice", "send", "alex/terminal", "--from", "alice/fresh", "first words from a new session")
out = cli("alex", "send", "alice/fresh", "answered at once")
check("a brand-new session that just messaged can be answered at once", "→ alice/fresh" in out and "no session" not in out, out)
out = cli("alice", "listen", "--as", "fresh", "--wait", "5")
check("…and it gets the answer", "answered at once" in out, out)
cli("alex", "listen", "--as", "terminal", "--wait", "0")
out = cli("alex", "send", "*", "broadcast from alex (e2e test, ignore)")
check("broadcast reaches alice's session", "alice/work-codex" in out, out)
codex.tool("check_inbox", {}, meta)
out = cli("alex", "send", "nobody", "x")
check("unknown name fails helpfully", "no session called 'nobody'" in out, out)

# --- a session waiting in check_inbox gets the reply there, once ---
import threading
waited = {}
t = threading.Thread(target=lambda: waited.setdefault("out", codex.tool("check_inbox", {"wait_secs": 15}, meta)))
t.start()
time.sleep(2)
out = cli("alex", "send", "alice/work-codex", "reply while you wait")
t.join()
check("long-poll returns the message that arrives", "reply while you wait" in waited.get("out", ""), waited)

# --- Codex hooks: session via hooks, delivery via codex queue ---
start = hook("alice", "codex", "session-start", {"session_id": "22222222-bbbb", "cwd": ROOT, "hook_event_name": "SessionStart"})
codex_name = os.path.basename(ROOT).lower() + "-codex"
check("codex SessionStart hook introduces the session", "SessionStart" in start and f"alice/{codex_name}" in start, start)
time.sleep(3)
out = cli("alex", "send", f"alice/{codex_name}", "please rebase on main")
check("message to a hooked codex session goes to codex queue", "queued into the Codex session" in out, out)
time.sleep(1.5)
log = open(QUEUE_LOG).read() if os.path.exists(QUEUE_LOG) else ""
check("codex queue got the thread and the message", "queue --thread 22222222-bbbb --message" in log and "please rebase on main" in log, log)
out = hook("alice", "codex", "post-tool-use", {"session_id": "22222222-bbbb", "cwd": ROOT, "hook_event_name": "PostToolUse"})
check("a message delivered by codex queue isn't injected again by the hook", out == "", out)

# --- hold policy ---
cfg = os.path.join(H["alice"], "config.toml")
text = open(cfg).read()
text = text.replace("[inbound.people]", '[inbound.people]\nalex = "hold"', 1) if "[inbound.people]" in text else text + '\n[inbound.people]\nalex = "hold"\n'
open(cfg, "w").write(text)
cli("alice", "stop")
time.sleep(3)
cli("alice", "team")
time.sleep(5)
out = cli("alex", "send", "alice/work-codex", "held message")
check("message to alice is held by her policy", "held until alice approves" in out, out)
out = codex.tool("check_inbox", {}, meta)
check("held message is not delivered", "held message" not in out, out)
out = cli("alice", "held")
check("tincan held lists it", "held message" in out, out)
cli("alice", "approve", "--all")
out = codex.tool("check_inbox", {}, meta)
check("approved message is delivered", "held message" in out, out)

# --- offline machine: outbox ---
codex.close()
cli("alice", "stop")
time.sleep(1)
out = cli("alex", "send", "alice/work-codex", "sent while you were offline")
check("message to an offline machine is queued", "not connected" in out or "no answer" in out, out)
cli("alice", "team")  # restarts alice's daemon
time.sleep(8)
cli("alice", "approve", "--all")  # alice still holds messages from alex (the hold policy above)
codex = Mcp("alice", "codex-mcp-client", strip_claude=True, cwd=os.path.join(D, "work"))
out = codex.tool("check_inbox", {}, meta)
check("outbox delivers when the machine is back", "sent while you were offline" in out, out)
codex.close()

# --- removal while away ---
cli("alice", "stop")
out = cli("alex", "remove", "alice")
check("remove alice (while her machine is off)", "Removed alice on desktop" in out, out)
check("removing someone replaces the team link", "team link changed" in out, out)
out = cli("alex", "link")
check("the new link differs from the old one", link.group(1) not in out, out)
cli("alice", "team")  # alice's machine comes back and dials alex
time.sleep(10)
out = cli("alice", "team")
check("alice's machine learns it was removed when it reconnects", "not in a team" in out, out)

claude.close()
cli("alex", "stop")
cli("alice", "stop")
print(f"\n{sum(results)}/{len(results)} checks passed  (state in {D})")
raise SystemExit(0 if all(results) else 1)
