"""Hook flows for Cursor, Gemini, Copilot and opencode against one isolated daemon.

    cargo build && python3 tests/e2e/hooks.py
"""
import json, os, subprocess, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "tincan")
D = tempfile.mkdtemp(prefix="tincan-hooks-")
env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
env.update(TINCAN_HOME=os.path.join(D, "home"), TINCAN_NO_MDNS="1", TINCAN_NO_SERVICE="1", TINCAN_MACHINE="box")
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
run("init", "hooks", "--as", "alex", "--no-setup")

# Cursor: conversation_id + workspace_roots; prompt-submit can't show context.
cur = {"conversation_id": "c-1", "workspace_roots": [proj], "hook_event_name": "sessionStart"}
out = hook("cursor", "session-start", cur)
check("cursor start → additional_context intro", '"additional_context"' in out and "alex/proj-cursor" in out, out)
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

# What a session is doing comes from its transcript (title, todo in progress), never from the agent reporting it.
tp = os.path.join(D, "cursor-transcript.jsonl")
with open(tp, "w") as f:
    f.write(json.dumps({"role": "assistant", "tool": "todo_write", "args": {"todos": [
        {"content": "Read the config loader", "status": "completed"},
        {"content": "Rename the config loader", "status": "in_progress"}]}}) + "\n")
hook("cursor", "post-tool-use", dict(cur, transcript_path=tp))
out = run("who", "proj-cursor")
check("the todo in progress in the transcript shows as what the session is doing", "↳ Rename the config loader" in out, out)
with open(tp, "a") as f:
    f.write(json.dumps({"args": {"todos": [{"content": "Rename the config loader", "status": "completed"}]}}) + "\n")
out = run("who", "proj-cursor")
check("…and goes away once it's done", "↳" not in out, out)

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
check("opencode poll prints the framed message as plain text", out.startswith("<tincan-message") and "opencode: ping" in out, out)
out = run("hook", "opencode", "poll", "--session", "o-1", cwd=oc)
check("…and nothing the next time", out == "", out)
run("hook", "opencode", "doing", "--session", "o-1", "--doing", "Flaky login test · Bisecting the failure", cwd=oc)
out = run("who", "oc-proj")
check("the opencode plugin reports title and todo", "↳ Flaky login test · Bisecting the failure" in out, out)

# pi: its extension reports busy/idle and the session's name, and asks for mail mid-run and when idle.
pi_dir = os.path.join(D, "pi-proj")
os.makedirs(pi_dir)
run("hook", "pi", "status", "--session", "pi-1", "--state", "busy", cwd=pi_dir)
run("hook", "pi", "doing", "--session", "pi-1", "--doing", "Auth refactor", cwd=pi_dir)
out = run("who", "pi-proj")
check("pi's extension registers the session, named for what it's doing", "pi-proj-pi" in out and "busy" in out and "↳ Auth refactor" in out, out)
out = run("send", "pi-proj-pi", "pi: stop and look at this")
check("the sender hears pi gets it within seconds", "within seconds" in out, out)
out = run("hook", "pi", "post-tool-use", "--session", "pi-1", cwd=pi_dir)
check("mid-run, pi gets it as plain text to steer in", out.startswith("<tincan-message") and "pi: stop and look at this" in out, out)

# Session end removes hook-only sessions.
hook("gemini", "session-end", gem)
out = run("who")
check("session-end removes the session", gem_name not in out, out)

# An unknown session can't read or send as the human.
out = run("hook", "cursor", "post-tool-use", stdin="{}")
check("a hook without a session id does nothing", out == "", out)

# Reason: its agent runs in the cloud; tincan lists the live sessions and sends with its CLI.
fake_reason, reason_log = os.path.join(D, "fake-reason.sh"), os.path.join(D, "reason.log")
listing = {"items": [
    {"session_id": "r-1", "status": "running", "title": "Fix flaky billing test", "repo": None, "branch": "reason/task-r1", "finished_at": None, "is_archived": False},
    {"session_id": "r-2", "status": "exit", "title": "Something finished", "finished_at": "2026-10-01T00:00:00Z", "is_archived": False},
]}
with open(fake_reason, "w") as f:
    f.write(f"""#!/bin/sh
if [ "$1 $2" = "session list" ]; then echo '{json.dumps(listing)}'; else printf '%s\\n' "$*" >> {reason_log}; fi
""")
os.chmod(fake_reason, 0o755)
cfg = os.path.join(env["TINCAN_HOME"], "config.toml")
with open(cfg) as f:
    text = f.read()
with open(cfg, "w") as f:
    f.write(text.replace("reason = false", "reason = true"))
env["TINCAN_REASON_BIN"] = fake_reason
run("stop")
run("who")
time.sleep(2)
out = run("who")
check("live Reason sessions are listed with their title, finished ones aren't",
      "fix-flaky-billing-reason" in out and "↳ Fix flaky billing test" in out and "Something finished" not in out, out)
out = run("send", "fix-flaky-billing-reason", "reason: the billing test is flaky on main too")
time.sleep(1)
sent = open(reason_log).read() if os.path.exists(reason_log) else ""
check("messages go in with `reason session send`", "queued into the Reason session" in out and sent.startswith("session send r-1 --message") and "flaky on main too" in sent, out + sent)
check("…telling it how to reply with the CLI", "tincan send alex --from alex/fix-flaky-billing-reason --reply-to" in sent, sent)
run("send", "alex", "--from", "alex/fix-flaky-billing-reason", "found it: a timezone bug")
out = run("inbox")
check("its reply comes from the Reason session", "── alex/fix-flaky-billing-reason" in out and "timezone bug" in out, out)

# The daemon being down never breaks the agent.
run("stop")
bad = dict(env, TINCAN_HOME="/nonexistent/dir/that/cannot/exist")
r = subprocess.run([BIN, "hook", "codex", "post-tool-use"], env=bad, input='{"session_id":"x"}', capture_output=True, text=True, timeout=20)
check("hook exits 0 and prints nothing when tincan is broken", r.returncode == 0 and r.stdout == "", f"{r.returncode} {r.stdout!r}")
print(f"\n{sum(results)}/{len(results)} hook checks passed")
raise SystemExit(0 if all(results) else 1)
