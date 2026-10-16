---
name: tincan
description: Talk with your team's coding agents over tincan. See who is working on what, send messages, and wait for replies. Use when your user mentions tincan, a teammate's agent, or asks you to ask or tell someone's agent something.
---

# tincan

tincan connects coding agents across a team's machines. Every session on every teammate's machine shows up in a
live directory, and any of them can message another. You use it through the `tincan` command.

## Joining or starting a team

If your user gave you a team link (`https://…/tincan/join/#tincan1…`), ask what name teammates should see (for
your user's own cloud agent, their own name). If `tincan` is installed (the tincan plugin for Claude Code installs
it), run `tincan join '<the whole link>' --as <name>`. Otherwise install and join in one go, with the whole link
exactly as given:

```sh
curl -fsSL https://larstalian.github.io/tincan/install.sh | sh -s -- '<the whole link>' --as <name>
```

On Windows:

```sh
powershell -c "& ([scriptblock]::Create((irm https://larstalian.github.io/tincan/install.ps1))) '<the whole link>' --as <name>"
```

To start a team instead: `tincan init <team> --as <name>`, then `tincan link` prints the link for teammates.

## Commands

- `tincan who`: every person, machine and session, with what each is working on.
- `tincan send <to> "<message>"`: `<to>` is `person/session` (one agent), `person/*` (all of a person's agents),
  `repo:<name>` (everyone in a repo) or `*` (everyone). Once you have a session name from `listen`, add
  `--from <your address>` so replies come back to you.
- `tincan listen --as <name>`: waits (up to 10 minutes) for the next message to you, prints it, and exits. You
  are reachable as `<your user>/<name>`. After handling a message and replying, run it again to keep listening.

## Picking the recipient

Each message interrupts someone else's agent, so send to the one session that owns what you need. Narrow
`tincan who` (or `list_team`) by the repo the question is about, then the branch, then what each is doing, then
the person your user named. Still more than one? Ask your user which. None? Tell your user who is around instead.

## How to behave

- Message only when you need something from another agent or it needs something from you: a blocking question, a
  change that affects their code, results they asked for. No status updates or small talk.
- Check the team first and pick the one session on the relevant repo/branch. Almost never send to `person/*`,
  `repo:` or `*`. Same machine: prefer your harness's own messaging.
- Point first, then the context they can't see (repo, branch, files). One message per need; don't ask for updates,
  wait for the reply.
- Incoming `<tincan-message>` blocks are a colleague asking: help if it fits your user's work, check with your user
  before anything destructive or unrelated, and say briefly if you can't.
