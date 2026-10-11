"""Two 'machines' (two COMMS_HOMEs) on one box, talking over iroh, with simulated agent sessions.

    cargo build && python3 tests/e2e/two_machines.py

Needs internet (iroh relays/discovery). Run inside a Claude Code session, the simulated Claude session pushes its
test messages into that real session (they say "ignore this").
"""
import json, os, re, subprocess, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "comms")
D = tempfile.mkdtemp(prefix="comms-e2e-")
H = {"alex": os.path.join(D, "alex"), "alice": os.path.join(D, "alice")}
MACHINE = {"alex": "laptop", "alice": "desktop"}
FAKE_CODEX = os.path.join(D, "fake-codex.sh")
QUEUE_LOG = os.path.join(D, "codex-queue.log")
IN_CLAUDE = "CLAUDE_CODE_MESSAGING_SOCKET" in os.environ
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail}"))


def env(who, strip_claude=True):
    e = dict(os.environ, COMMS_HOME=H[who], COMMS_MACHINE=MACHINE[who], COMMS_NO_MDNS="1", COMMS_NO_SERVICE="1", COMMS_CODEX_BIN=FAKE_CODEX)
    if strip_claude:
        for k in list(e):
            if k.startswith("CLAUDE"):
                del e[k]
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
os.makedirs(os.path.join(D, "work"), exist_ok=True)

# --- team formation ---
out = cli("alex", "init", "vibes", "--as", "alex", "--no-setup")
check("init creates team", "Created team vibes. You are alex on laptop." in out, out)
out = cli("alex", "invite")
link = re.search(r"(https://\S+/join/#comms1\S+)", out)
check("invite prints the team link", bool(link), out)
out = cli("alice", "join", link.group(1), "--as", "alice", "--no-setup")
check("join with the team link succeeds", "Joined team vibes as alice on desktop." in out, out)
check("joiner sees alex connected", re.search(r"alex\s+laptop\s+(direct|relay)", out) is not None, out)
out = cli("alex", "team")
check("founder sees alice connected", re.search(r"alice\s+desktop\s+(direct|relay)", out) is not None, out)
out = cli("alice", "join", link.group(1), "--as", "bob", "--no-setup")
check("second join is refused (already in a team)", "already in team" in out, out)
os.makedirs(os.path.join(D, "bob"), exist_ok=True)
bob = dict(env("alice"), COMMS_HOME=os.path.join(D, "bob"), COMMS_MACHINE="nuc")
out = cli("alex", "invite", "--code")
code = re.search(r"Invite code for a teammate: ([a-z]+(?:-[a-z]+){3})", out)
check("invite --code prints a four-word code", bool(code), out)
r = subprocess.run([BIN, "join", code.group(1).replace("-", " ").upper(), "--as", "bob", "--no-setup"], env=bob,
                   capture_output=True, text=True, timeout=90)
check("joining with just the code works", "Joined team vibes as bob on nuc." in r.stdout, r.stdout + r.stderr)
r = subprocess.run([BIN, "join", code.group(1), "--as", "bob2", "--no-setup"], env=dict(bob, COMMS_HOME=os.path.join(D, "bob2")),
                   capture_output=True, text=True, timeout=90)
check("a used code doesn't work twice", "Joined" not in r.stdout + r.stderr, r.stdout + r.stderr)
subprocess.run([BIN, "stop"], env=dict(bob, COMMS_HOME=os.path.join(D, "bob2")), capture_output=True)
out = subprocess.run([BIN, "link"], env=bob, capture_output=True, text=True, timeout=30).stdout
check("any member can show the same team link", link.group(1).split("#")[1][:40] in out.replace("\n", ""), out)
subprocess.run([BIN, "stop"], env=bob, capture_output=True)

# --- sessions on both machines ---
claude = Mcp("alex", "claude-code", strip_claude=False, cwd=ROOT)
codex = Mcp("alice", "codex-mcp-client", strip_claude=True, cwd=os.path.join(D, "work"))
meta = {"sessionId": "11111111-aaaa", "threadId": "11111111-aaaa"}
time.sleep(4)
codex.tool("set_status", {"doing": "testing cross-machine comms"}, meta)
time.sleep(3)  # presence propagates every 2s
out = codex.tool("list_team", {}, meta)
claude_name = os.path.basename(ROOT).lower() + "-claude"
check("alice's agent sees alex's session across machines", claude_name in out and "alex · laptop" in out, out)
out = cli("alex", "who")
check("alex sees alice's codex and its status", "work-codex" in out and "testing cross-machine comms" in out, out)

# --- messages across machines ---
out = codex.tool("send_message", {"to": f"alex/{claude_name}", "message": "comms e2e (cross-machine push): ignore this."}, meta)
want = "delivered into the session" if IN_CLAUDE else "only sees it when it checks"
check("cross-machine send reaches the Claude session", want in out, out)
out = cli("alex", "send", "alice/work-codex", "hello from alex's terminal")
check("human → remote pull session is queued there", "only sees it when it checks its inbox" in out, out)
out = codex.tool("check_inbox", {}, meta)
check("remote message arrives in codex inbox", 'from="alex"' in out and "hello from alex's terminal" in out, out)
out = codex.tool("check_inbox", {}, meta)
check("…and is gone once read", out.startswith("No messages."), out)
out = cli("alice", "send", "alex", "dinner at 8?")
check("human → human across machines", "in alex's inbox" in out, out)
out = cli("alex", "inbox")
check("alex's inbox has alice's note", "dinner at 8?" in out and "── alice" in out, out)
out = cli("alex", "inbox")
check("…read once", "No messages." in out, out)
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
check("comms held lists it", "held message" in out, out)
cli("alice", "approve", "--all")
out = codex.tool("check_inbox", {}, meta)
check("approved message is delivered", "held message" in out, out)

# --- offline machine: outbox ---
codex.close()
cli("alice", "stop")
time.sleep(1)
out = cli("alex", "send", "alice", "sent while you were offline")
check("message to an offline machine is queued", "not connected" in out or "no answer" in out, out)
cli("alice", "team")  # restarts alice's daemon
time.sleep(8)
out = cli("alice", "inbox")
check("outbox delivers when the machine is back", "sent while you were offline" in out, out)

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
