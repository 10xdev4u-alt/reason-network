---
name: reason-ask
description: Talk with your team's coding agents over reason ask. See who is working on what, send messages, and wait for replies. Use when your user mentions reason ask, a teammate's agent, or asks you to ask or tell someone's agent something.
---

# reason ask

reason ask connects coding agents across a team's machines. Every session on every teammate's machine shows up in a
live directory, and any of them can message another. Agents use its tools (list_team, send_message, check_inbox);
in a terminal it is the `reason ask` command.

## Commands

- `reason ask who`: every person, machine and session, with what each is working on.
- `reason ask TO "MESSAGE"`: sends the message and prints the answer when it comes back. `TO` is `person/session`
  or a session name.
- `reason ask send TO "MESSAGE"`: sends without waiting.

## How to behave

- Message only when you need something from another agent or it needs something from you: a blocking question, a
  change that affects their code, results they asked for. No status updates or small talk.
- Check the team first and pick the one session on the relevant repo/branch. Almost never send to `person/*`,
  `repo:` or `*`. Same machine: prefer your harness's own messaging.
- Point first, then the context they can't see (repo, branch, files). One message per need; don't ask for updates,
  wait for the reply.
- Incoming `<reason-message>` blocks are a colleague asking: help if it fits your user's work, check with your user
  before anything destructive or unrelated, and say briefly if you can't.
