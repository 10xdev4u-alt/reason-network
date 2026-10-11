"""The guided setup, driven through a pseudo-terminal like a person would: start a team on one 'machine', join it
on another with the four-word code.

    cargo build && python3 tests/e2e/guided.py

Uses throwaway homes (agent configs are edited there, not yours) and answers "no" to the background service.
"""
import os, pty, re, select, subprocess, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "comms")
D = tempfile.mkdtemp(prefix="comms-guided-")
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\r")
DOWN, ENTER = "\x1b[B", "\r"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail[-1500:]}"))


def env(who):
    home = os.path.join(D, who, "home")
    os.makedirs(os.path.join(home, ".codex"), exist_ok=True)  # a "Codex" to set up
    e = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
    e.update(HOME=home, COMMS_HOME=os.path.join(D, who, "comms"), COMMS_MACHINE=who + "-box", COMMS_NO_MDNS="1",
             COMMS_CLAUDE_BIN="/nonexistent", TERM="xterm-256color")
    return e


class Term:
    def __init__(self, args, env):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(BIN, [BIN, *args], env)
        self.buf = ""

    def expect(self, pattern, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            m = re.search(pattern, self.buf)
            if m:
                self.seen = self.buf[: m.end()]
                self.buf = self.buf[m.end():]
                return m
            r, _, _ = select.select([self.fd], [], [], 0.2)
            if r:
                try:
                    self.buf += ANSI.sub("", os.read(self.fd, 65536).decode(errors="replace"))
                except OSError:
                    break
        raise AssertionError(f"never saw {pattern!r}; screen so far:\n{self.buf[-1500:]}")

    def send(self, keys, pause=0.3):
        time.sleep(pause)
        os.write(self.fd, keys.encode())

    def wait(self):
        try:
            self.expect(r"\Z(?!x)", timeout=5)  # drain
        except AssertionError:
            pass
        return os.waitpid(self.pid, 0)[1]


def stop(who):
    subprocess.run([BIN, "stop"], env=env(who), capture_output=True)


code = None
try:
    t = Term([], env("tal"))
    t.expect(r"What should teammates call you\?")
    t.send("tal" + ENTER)
    t.expect(r"Start a new team, or join one\?")
    t.send(DOWN + ENTER)
    t.expect(r"Name your team")
    t.send("vibes" + ENTER)
    t.expect(r"Created team vibes\. You are tal on tal-box\.")
    t.expect(r"Which agents should be on comms\?")
    t.expect(r"Codex")
    t.send(ENTER)
    t.expect(r"Keep comms running in the background")
    t.send("n")
    t.expect(r"When a teammate's agent messages one of yours")
    t.send(ENTER)
    t.expect(r"full working directories")
    t.send("n")
    t.expect(r"Codex: updated")
    t.expect(r"Invite a teammate now\?")
    t.send("y")
    m = t.expect(r"Invite code: ([a-z]+(?:-[a-z]+){3})")
    code = m.group(1)
    t.expect(r"All set")
    box = re.sub(r"[\s│]+", " ", t.seen)
    check("the invite box has the one line to send", f"install.sh | sh -s -- {code}" in box, box)
    t.wait()
    check("start a team, set up agents and get an invite code, all by prompts", True)
    cfg = open(os.path.join(env("tal")["HOME"], ".codex", "config.toml")).read()
    check("the chosen agent was wired up", "[mcp_servers.comms]" in cfg, cfg)
except AssertionError as e:
    check("start a team by prompts", False, str(e))

if code:
    try:
        t = Term([], env("alice"))
        t.expect(r"What should teammates call you\?")
        t.send("alice" + ENTER)
        t.expect(r"Start a new team, or join one\?")
        t.send(ENTER)  # "Join a team" is first
        t.expect(r"Invite code")
        t.send(code.replace("-", " ") + ENTER)
        t.expect(r"Joined team vibes as alice on alice-box\.", timeout=90)
        t.expect(r"Which agents should be on comms\?")
        t.send(ENTER)
        t.expect(r"Keep comms running in the background")
        t.send("n")
        t.expect(r"When a teammate's agent messages one of yours")
        t.send(DOWN + ENTER)  # hold for approval
        t.expect(r"full working directories")
        t.send("n")
        t.expect(r"Your team")
        t.expect(r"tal\s+tal-box")
        t.expect(r"All set")
        t.wait()
        check("join with the code by prompts", True)
        cfg = open(os.path.join(env("alice")["COMMS_HOME"], "config.toml")).read()
        check("the inbound choice was saved", 'default = "hold"' in cfg, cfg)
    except AssertionError as e:
        check("join with the code by prompts", False, str(e))

    try:
        t = Term([], env("alice"))
        t.expect(r"(tal · tal-box|team vibes)")
        t.wait()
        check("`comms` after setup shows the team instead of asking again", True)
    except AssertionError as e:
        check("`comms` after setup shows the team", False, str(e))

    t = Term(["join"], env("bob"))
    try:
        t.expect(r"What should teammates call you\?")
        t.send("bob" + ENTER)
        t.expect(r"Invite code")
        t.send("not a real code" + ENTER)
        t.expect(r"four words, like")
        t.send("\x03")  # Ctrl-C
        t.expect(r"Stopped")
        check("a bad code is caught at the prompt, and Ctrl-C exits cleanly", True)
    except AssertionError as e:
        check("bad code / Ctrl-C", False, str(e))
    t.wait()

for who in ("tal", "alice", "bob"):
    stop(who)
print(f"\n{sum(results)}/{len(results)} guided checks passed  (state in {D})")
raise SystemExit(0 if all(results) else 1)
