"""The dashboard in a real browser: the board the daemon owns is rendered, a claim written over IPC shows up with
nobody reloading the page, and a person can take a task from the page itself.

    cargo build && python3 tests/e2e/dashboard.py [--shot /tmp/board.png]

Needs chromium (via playwright). It is not part of `ci.yml`: the other harnesses need nothing but python, and this
one is for looking at the thing, not for merging a change.
"""
import os, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "reason-network")
D = tempfile.mkdtemp(prefix="reason-network-dash-")
HOME = os.path.join(D, "home")
TOKEN = "tok-" + os.path.basename(D)
PORT = (lambda s: (s.bind(("127.0.0.1", 0)), s.getsockname()[1], s.close())[1])(socket.socket())
results = []
env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
env.update(REASON_NETWORK_HOME=HOME, REASON_NETWORK_NO_MDNS="1", REASON_NETWORK_NO_SERVICE="1", REASON_NETWORK_MACHINE="box")


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'PASS' if ok else 'FAIL'}: {name}" + ("" if ok else f"\n      {detail[-800:]}"))


def cli(*args, timeout=60):
    return subprocess.run([BIN, *args], env=env, capture_output=True, text=True, timeout=timeout)


os.makedirs(HOME, exist_ok=True)
with open(os.path.join(HOME, "config.toml"), "w") as f:
    f.write(f'web = true\nweb_bind = "127.0.0.1"\nweb_port = {PORT}\nweb_token = "{TOKEN}"\n')
cli("init", "hooks", "--as", "alex", "--no-setup")
cli("stop")
daemon = subprocess.Popen([BIN, "daemon"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)

# A task the daemon owns, put there with no browser involved at all.
task = cli("task", "add", "--title", "render the board", "--boundary", "a row, and a detail panel").stdout.strip()
check("a task can be added with no page open", len(task) > 10, task)

from playwright.sync_api import sync_playwright

shot = sys.argv[sys.argv.index("--shot") + 1] if "--shot" in sys.argv else None
# This machine's chromium if it has one, otherwise the one playwright downloads.
chrome = next((p for p in ("/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome") if os.path.exists(p)), None)
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=chrome, args=["--no-sandbox"])
    page = browser.new_page(viewport={"width": 1440, "height": 900})
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    page.goto(f"http://127.0.0.1:{PORT}/?token={TOKEN}")
    page.wait_for_selector("#board .task-row", timeout=15000)
    check("the board renders the task the daemon owns", "render the board" in page.inner_text("#board"), page.inner_text("#board"))
    check("nothing on the page throws", not errors, str(errors))
    check("the opened page has the token cookie, not the token in its URL", TOKEN not in page.url, page.url)

    # An agent claims it over IPC while the page is open: the page must show it without a reload.
    cli("task", "claim", task, "--as", "pi-2")
    row = page.locator("#board .task-row").filter(has_text="pi-2")
    try:
        # No reload, no polling from us: the daemon wakes the page. (String predicates would need 'unsafe-eval',
        # which the page's own policy refuses — that is the policy working.)
        row.wait_for(timeout=15000)
        check("a claim written over IPC appears without anyone reloading", True)
    except Exception as e:
        check("a claim written over IPC appears without anyone reloading", False, f"{e} {page.inner_text('#board')}")

    # Clicking the row opens the history and the actions.
    page.click("#board .task-row")
    page.wait_for_selector("#task-detail .task-history", timeout=10000)
    detail = page.inner_text("#task-detail")
    check("the selected task shows its history", "claimed" in detail and "pi-2" in detail, detail)

    # An empty box is not evidence: the page says so, and the daemon's state does not move.
    page.click("#task-actions button:has-text('close with evidence')")
    page.wait_for_selector("#toast:not(.hidden)", timeout=5000)
    check("closing with no evidence is refused", "evidence" in page.inner_text("#toast"), page.inner_text("#toast"))
    still = cli("task", "list", "--json").stdout
    check("…and the task is still open", '"state": "claimed"' in still, still)

    # The person takes work from the page: the daemon's row changes, and the page follows it.
    page.fill("#task-text", "ci green on the merge")
    page.click("#task-actions button:has-text('close with evidence')")
    try:
        # Closed work leaves the board, so the row going away is the page following the daemon.
        page.locator("#board .task-row").filter(has_text="render the board").first.wait_for(state="detached", timeout=15000)
        check("a person can close a task from the page, and the board follows", True, page.inner_text("#board"))
    except Exception as e:
        check("a person can close a task from the page, and the board follows", False, f"{e} {page.inner_text('#board')}")
    listing = cli("task", "list", "--all", "--json").stdout
    check("the close the page sent is the daemon's state, not the page's", '"state": "done"' in listing, listing)
    if shot:
        page.screenshot(path=shot, full_page=True)
        print(f"      shot: {shot}")
    browser.close()

daemon.terminate()
print(f"\n{sum(results)}/{len(results)} dashboard checks passed")
sys.exit(0 if all(results) else 1)
