---
name: swiss
description: Install and inspect the local swiss gateway only when the user explicitly asks; use swiss-remote for configured remote targets.
disable-model-invocation: true
---

# swiss

This skill is the entrance. The procedure is `SETUP.md` beside this file. If that file is not
there, run `swiss setup guide`. If this binary has no such command, read `SETUP.md` from the
release that matches `swiss --version`.

Act only after the user explicitly asks to install, update, configure, or check swiss. Until
they pick one of those, run `swiss --version` and `swiss status` and stop.

Never print `swiss token` or `swiss creds`. Never run `swiss export`. Never put a secret's
value in a manifest or on a command line. Run `swiss skill list` and ask which skills to
install; installing none is valid. For remote commands use the `swiss-remote` skill, and only
after a separate explicit request.
