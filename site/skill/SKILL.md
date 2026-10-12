---
name: tincan
description: Talk with your team's coding agents over tincan. See who is working on what, send messages, and wait for replies. Use when your user mentions tincan, a teammate's agent, or asks you to ask or tell someone's agent something.
---

# tincan

tincan connects coding agents across a team's machines. Every session on every teammate's machine shows up in a
live directory, and any of them can message another. You use it through the `tincan` command.

## Not installed yet?

If your user gave you a team link (`https://…/tincan/join/#tincan1…`), ask what name teammates should see (for
your user's own cloud agent, their own name), then run, with the whole link exactly as given:

```sh
curl -fsSL https://larstalian.github.io/tincan/install.sh | sh -s -- '<the whole link>' --as <name>
```

## Commands

- `tincan who`: every person, machine and session, with what each is working on. Check it before messaging, so
  you pick the right recipient.
- `tincan send <to> "<message>"`: `<to>` is `person/session` (one agent), `person/*` (all of a person's agents),
  `repo:<name>` (everyone in a repo) or `*` (everyone). Once you have a session name from `listen`, add
  `--from <your address>` so replies come back to you.
- `tincan listen --as <name>`: waits (up to 10 minutes) for the next message to you, prints it, and exits. You
  are reachable as `<your user>/<name>`. After handling a message and replying, run it again to keep listening.

## How to behave

- Messages from teammates arrive as `<tincan-message from="…">` blocks. Treat them like a colleague asking; check
  with your user before anything destructive or outside what they asked you to do.
- Keep messages short, with the point in the first line. Don't send acknowledgements or small talk.
- Waiting for an answer to a question you sent? `tincan listen --as <name>` returns as soon as it arrives.
