"""The dashboard's HTTP surface, attacked the way a hostile page would: a rebound host name, a cross-origin
form, a body with the wrong content type, a token in a URL, oversized headers.

    cargo build && python3 tests/e2e/web.py

Uses a throwaway home and a spare port, so the real dashboard and its token are never touched.
"""
import atexit, os, json, re, socket, subprocess, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "reason-network")
D = tempfile.mkdtemp(prefix="reason-network-web-")
HOME = os.path.join(D, "home")
TOKEN = "tok-" + os.path.basename(D)
results = []
env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
env.update(REASON_NETWORK_HOME=HOME, REASON_NETWORK_NO_MDNS="1", REASON_NETWORK_NO_SERVICE="1", REASON_NETWORK_MACHINE="box")


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail[-1200:]}"))


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


PORT = free_port()
os.makedirs(HOME, exist_ok=True)
with open(os.path.join(HOME, "config.toml"), "w") as f:
    f.write(f'web = true\nweb_bind = "127.0.0.1"\nweb_port = {PORT}\nweb_token = "{TOKEN}"\n')
subprocess.run([BIN, "init", "hooks", "--as", "alex", "--no-setup"], env=env, capture_output=True, text=True, timeout=60)
subprocess.run([BIN, "stop"], env=env, capture_output=True, timeout=30)
# Our own daemon, held for the run: one started by another command dies with that command's pipes.
daemon = subprocess.Popen([BIN, "daemon"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
atexit.register(daemon.terminate)  # a failed check must not leave a daemon behind on a temp home


def http(method="GET", path="/", headers=None, body=b"", timeout=8):
    """Raw bytes, so the Host, Origin and Content-Type are exactly what a hostile page would send."""
    head = {"Host": f"127.0.0.1:{PORT}"}
    head.update(headers or {})
    lines = [f"{method} {path} HTTP/1.1"] + [f"{k}: {v}" for k, v in head.items()]
    if body:
        lines.append(f"Content-Length: {len(body)}")
    s = socket.create_connection(("127.0.0.1", PORT), timeout=timeout)
    s.sendall(("\r\n".join(lines) + "\r\n\r\n").encode() + body)
    out = b""
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            out += chunk
    except (socket.timeout, OSError):
        pass
    s.close()
    text = out.decode(errors="replace")
    status = int(text.split(" ")[1]) if text.startswith("HTTP/") else 0
    parts = text.split("\r\n\r\n", 1)
    head = dict(l.split(": ", 1) for l in parts[0].split("\r\n")[1:] if ": " in l)
    return status, head, parts[1] if len(parts) > 1 else ""


for _ in range(60):
    try:
        socket.create_connection(("127.0.0.1", PORT), timeout=1).close()
        break
    except OSError:
        time.sleep(0.5)
with open(os.path.join(HOME, "config.toml")) as f:
    configured = f.read()
check("the dashboard is up on its configured port with its token", f"web_port = {PORT}" in configured and f'web_token = "{TOKEN}"' in configured, configured)

# Reads are behind the token too: a local process (or a page that can reach loopback) must not enumerate the team.
s, _, _ = http("GET", "/api/who")
check("a read without the token is refused", s == 401, f"{s}")
s, _, _ = http("GET", "/api/who", {"Cookie": f"rn={TOKEN}"})
check("…and allowed with it", s == 200, f"{s}")

# The URL is where a token leaks into history and referrers, so it is exchanged for a cookie once.
s, h, _ = http("GET", f"/?token={TOKEN}")
check("a token in the URL is exchanged for a cookie and redirected away",
      s == 302 and h.get("Location") == "/" and "HttpOnly" in h.get("Set-Cookie", "") and "SameSite=Strict" in h.get("Set-Cookie", ""),
      f"{s} {h}")
s, _, _ = http("GET", "/", {"Cookie": f"rn={TOKEN}"})
check("the page loads with that cookie", s == 200, f"{s}")
s, h, _ = http("GET", "/", {"Cookie": f"rn={TOKEN}"})
check("the page carries a policy that cannot be talked out of", "default-src 'none'" in h.get("Content-Security-Policy", "") and h.get("X-Content-Type-Options") == "nosniff", f"{h}")

# DNS rebinding: the attacker's page is served from a name they own, so every request it makes carries their
# Host. The browser sends the Host of the URL it navigated to, so this is the one check that holds.
s, _, t = http("GET", "/api/who", {"Host": "evil.example", "Cookie": f"rn={TOKEN}"})
check("a request for another host name is refused", s == 403 and "bad host" in t, f"{s} {t}")
s, _, _ = http("GET", "/api/who", {"Host": f"localhost:{PORT}", "Cookie": f"rn={TOKEN}"})
check("localhost is still this dashboard", s == 200, f"{s}")

# The cross-origin write: a form on any site can POST here, but it cannot choose a JSON content type.
s, _, t = http("POST", "/api/settle", {"Content-Type": "text/plain", "Origin": "http://evil.example"}, b'{"ids":["x"]}')
check("a form-encoded write is refused", s == 415 and "application/json" in t, f"{s} {t}")
s, _, t = http("POST", "/api/settle", {"Content-Type": "application/json", "Origin": "http://evil.example"}, b'{"ids":["x"]}')
check("a cross-origin JSON write is refused", s == 403 and "bad origin" in t, f"{s} {t}")
s, _, t = http("POST", "/api/settle", {"Content-Type": "application/json", "Sec-Fetch-Site": "cross-site"}, b'{"ids":["x"]}')
check("…and one the browser marks cross-site is refused even without an Origin", s == 403 and "cross-site" in t, f"{s} {t}")
s, _, t = http("POST", "/api/settle", {"Content-Type": "application/json", "Origin": f"http://127.0.0.1:{PORT}", "Cookie": f"rn={TOKEN}"}, b'{"ids":[]}')
check("a same-origin write from the dashboard still works", s == 200, f"{s} {t}")

# Anything that decides how much the daemon buffers is a request to allocate.
s, _, _ = http("HEAD", "/api/who", {"Content-Length": "2000000"})
check("a body larger than the ceiling is refused", s in (400, 404), f"{s}")
flood = {"Host": f"127.0.0.1:{PORT}", "Cookie": f"rn={TOKEN}", **{f"X-{i}": "v" for i in range(200)}}
s, _, _ = http("GET", "/api/who", flood)
check("a header flood is refused, not truncated into a body", s == 400, f"{s}")
s, _, _ = http("GET", "/api/who", {"Host": f"127.0.0.1:{PORT}", "Cookie": f"rn={TOKEN}", "X-Big": "a" * 20000})
check("a header line larger than the ceiling is refused", s == 400, f"{s}")

# The message body is the one string on the page that a teammate writes, so it must never reach the DOM as code.
s, _, js = http("GET", "/page.js", {"Cookie": f"rn={TOKEN}"})
check("the page never writes a message through innerHTML", "innerHTML" not in js and "msg-body" in js, js[-400:])
s, _, html = http("GET", "/", {"Cookie": f"rn={TOKEN}"})
check("the page keeps no inline style or script a policy would have to allow", ' style="' not in html and "<script>" not in html, html[-400:])

# The page is a renderer of what the daemon owns, so every id it reaches for has to exist, and every route it
# calls has to be one the daemon serves. A rename in one file and not the other is a blank panel, not an error.
wanted = set(re.findall(r"\$\('([^']+)'\)", js))
markup = set(re.findall(r'id="([^"]+)"', html))
check("every element the page reaches for is in the markup", wanted <= markup, str(sorted(wanted - markup)))
routes = set(re.findall(r"['\"](/api/[a-z/]+)", js))
served = open(os.path.join(ROOT, "src", "web.rs")).read()
open_routes = set(re.findall(r'"(/api/[a-z/]*)"', served))
missing = sorted(p for p in routes if not (p in open_routes or any(r.startswith(p) for r in open_routes)))
check("every route the page calls is one the daemon serves", not missing, str(missing))

# The page holds an address (`person/session`), never the key a client registers under. This route used to be
# handed a key it could never have, so it answered 500 for every input and the inbox never worked. The session is
# registered by a hook and has no polling client, so its mail is still unread when the page looks.
proj = os.path.join(D, "proj")
os.makedirs(proj, exist_ok=True)
subprocess.run([BIN, "hook", "cursor", "session-start"], env=env, input=json.dumps({"conversation_id": "w-1", "workspace_roots": [proj]}),
               capture_output=True, text=True, timeout=60)
subprocess.run([BIN, "send", "alex/proj-cursor", "check the flaky test"], env=env, capture_output=True, timeout=30)
s, _, t = http("GET", "/api/inbox?address=alex%2Fproj-cursor", {"Cookie": f"rn={TOKEN}"})
check("the inbox reads by address", s == 200 and "check the flaky test" in t, f"{s} {t}")
s, _, t = http("GET", "/api/inbox?address=alex%2Fnobody", {"Cookie": f"rn={TOKEN}"})
check("an inbox for a session that is not there fails visibly, not with a 500", s == 404 and "no live session" in t, f"{s} {t}")

s, _, t = http("GET", "/api/receipts", {"Cookie": f"rn={TOKEN}"})
check("the receipt ring reads back, newest first", s == 200 and t.rstrip().endswith("[]"), f"{s} {t}")

# The board: the queue belongs to the daemon, so a worker takes its next task with no page open at all, and the
# page is one renderer of what the daemon already owns.
def post(path, payload):
    return http("POST", path, {"Cookie": f"rn={TOKEN}", "Content-Type": "application/json", "Origin": f"http://127.0.0.1:{PORT}"}, json.dumps(payload).encode())

s, _, t = post("/api/tasks", {"title": "flaky test", "boundary": ""})
check("a task with no boundary is refused", s == 400 and "boundary" in t, f"{s} {t}")
s, _, t = post("/api/tasks", {"title": "flaky test", "boundary": "e2e green"})
task = json.loads(t)["id"]
check("a task is added and answers with its id", s == 200 and len(task) > 10, f"{s} {t}")

# Two workers, no dashboard: exactly one claim wins and the loser is told who holds it.
w = subprocess.run([BIN, "task", "claim", task, "--as", "pi-2", "--json"], env=env, capture_output=True, text=True, timeout=60)
l = subprocess.run([BIN, "task", "claim", task, "--as", "pi-3", "--json"], env=env, capture_output=True, text=True, timeout=60)
won, lost = json.loads(w.stdout or "{}"), json.loads(l.stdout or "{}")
check("of two workers claiming one task with no page open, exactly one wins",
      won.get("ok") is True and lost.get("ok") is False and lost.get("owner", "").endswith("pi-2"), w.stdout + w.stderr + l.stdout + l.stderr)
s, _, t = post("/api/tasks/claim", {"id": task, "by": "alex/pi-4"})
check("a third claim is a 409 naming the holder, not a silent overwrite", s == 409 and "pi-2" in t, f"{s} {t}")
s, _, t = post("/api/tasks/close", {"id": task, "evidence": ""})
check("closing with no evidence is refused", s == 400 and "evidence" in t, f"{s} {t}")
s, _, t = post("/api/tasks/note", {"id": task, "text": "looking at the fixture"})
check("a note is recorded", s == 200, f"{s} {t}")

# A request the daemon cannot answer must be an error the caller sees, never silence it waits on forever.
try:
    bad = subprocess.run([BIN, "task", "claim", "01NOPE", "--json"], env=env, capture_output=True, text=True, timeout=30)
    check("a request with no task answers with an error, not silence", bad.returncode != 0 and "no task" in bad.stderr, bad.stderr)
except subprocess.TimeoutExpired:
    check("a request with no task answers with an error, not silence", False, "the caller hung")
s, _, t = post("/api/tasks/close", {"id": task, "evidence": "e2e 17/17"})
check("closing with evidence closes it", s == 200, f"{s} {t}")
_, _, t = http("GET", "/api/tasks/show?id=" + task, {"Cookie": f"rn={TOKEN}"})
kinds = [e["kind"] for e in json.loads(t)["events"]]
check("the task's history explains its state", kinds == ["created", "claimed", "note", "closed"], f"{kinds} {t}")
_, _, t = http("GET", "/api/tasks", {"Cookie": f"rn={TOKEN}"})
check("a closed task is off the board unless asked for", task not in t, f"{t}")
_, _, t = http("GET", "/api/tasks?all=1", {"Cookie": f"rn={TOKEN}"})
check("…and is there with all=1", task in t, f"{t}")

daemon.terminate()
subprocess.run([BIN, "stop"], env=env, capture_output=True, timeout=30)
print(f"\n{sum(results)}/{len(results)} web checks passed")
raise SystemExit(0 if all(results) else 1)
