"""Hook flows for Cursor, Gemini, Copilot and opencode against one isolated daemon.

    cargo build && python3 tests/e2e/hooks.py
"""
import json, os, subprocess, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "comms")
D = tempfile.mkdtemp(prefix="comms-hooks-")
env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
env.update(COMMS_HOME=os.path.join(D, "home"), COMMS_NO_MDNS="1", COMMS_MACHINE="box")
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail}"))


def run(*args, stdin=None, cwd=None):
    r = subprocess.run([BIN, *args], env=env, capture_output=True, text=True, input=stdin, timeout=30, cwd=cwd)
    return (r.stdout + r.stderr).strip()


def hook(agent, event, payload):
    return run("hook", agent, event, stdin=json.dumps(payload))


proj = os.path.join(D, "proj")
os.makedirs(proj)
run("init", "hooks", "--as", "tal", "--no-setup")

# Cursor: conversation_id + workspace_roots; prompt-submit can't show context.
cur = {"conversation_id": "c-1", "workspace_roots": [proj], "hook_event_name": "sessionStart"}
out = hook("cursor", "session-start", cur)
check("cursor start → additional_context intro", '"additional_context"' in out and "tal/proj-cursor" in out, out)
run("send", "proj-cursor", "cursor, please look at PR 12")
out = hook("cursor", "user-prompt-submit", cur)
check("cursor prompt-submit prints nothing and keeps the mail", out == "", out)
out = hook("cursor", "post-tool-use", cur)
check("cursor post-tool-use injects the message", '"additional_context"' in out and "PR 12" in out, out)
out = hook("cursor", "post-tool-use", cur)
check("…only once", out == "", out)
run("send", "proj-cursor", "--fyi", "fyi: deploy at 5")
out = hook("cursor", "post-tool-use", cur)
check("FYI messages wait for a pause (not injected mid-turn)", out == "", out)
out = hook("cursor", "stop", cur)
check("cursor stop → followup_message with the FYI", '"followup_message"' in out and "deploy at 5" in out, out)

# Gemini
gem = {"session_id": "g-1", "cwd": D, "hook_event_name": "SessionStart"}
hook("gemini", "session-start", gem)
gem_name = os.path.basename(D).lower().replace("_", "-") + "-gemini"
run("send", gem_name, "gemini: run the tests")
out = hook("gemini", "stop", gem)
check("gemini stop → decision block with the message", '"decision":"block"' in out and "run the tests" in out, out)

# Copilot: camelCase sessionId
cop = {"sessionId": "p-1", "cwd": proj, "timestamp": 1}
out = hook("copilot", "session-start", cop)
check("copilot start → additionalContext intro", '"additionalContext"' in out and "proj-copilot" in out, out)
run("send", "proj-copilot", "copilot: hello")
out = hook("copilot", "post-tool-use", cop)
check("copilot post-tool-use → additionalContext", '"additionalContext"' in out and "copilot: hello" in out, out)

# opencode plugin calls: status + poll
oc = os.path.join(D, "oc-proj")
os.makedirs(oc)
run("hook", "opencode", "status", "--session", "o-1", "--state", "busy", cwd=oc)
out = run("who", "oc-proj")
check("opencode status registers a busy session", "oc-proj-opencode" in out and "busy" in out, out)
run("send", "oc-proj-opencode", "opencode: ping")
out = run("hook", "opencode", "poll", "--session", "o-1", cwd=oc)
check("opencode poll prints the framed message as plain text", out.startswith("<comms-message") and "opencode: ping" in out, out)
out = run("hook", "opencode", "poll", "--session", "o-1", cwd=oc)
check("…and nothing the next time", out == "", out)

# Session end removes hook-only sessions.
hook("gemini", "session-end", gem)
out = run("who")
check("session-end removes the session", gem_name not in out, out)

# An unknown session can't read or send as the human.
out = run("hook", "cursor", "post-tool-use", stdin="{}")
check("a hook without a session id does nothing", out == "", out)

# The daemon being down never breaks the agent.
run("stop")
bad = dict(env, COMMS_HOME="/nonexistent/dir/that/cannot/exist")
r = subprocess.run([BIN, "hook", "codex", "post-tool-use"], env=bad, input='{"session_id":"x"}', capture_output=True, text=True, timeout=20)
check("hook exits 0 and prints nothing when comms is broken", r.returncode == 0 and r.stdout == "", f"{r.returncode} {r.stdout!r}")
print(f"\n{sum(results)}/{len(results)} hook checks passed")
raise SystemExit(0 if all(results) else 1)
