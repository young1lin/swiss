---
name: swiss-brief
description: >-
  What this machine offers an agent through the local swiss gateway, and the one way to run heavy
  work on it. Use before running a build, a test suite or any other command that loads every core
  (cargo build/test/clippy, npm test, vitest, a release script), when other agents may be working
  on the same machine, when you need to start, stop or read the logs of an app swiss manages,
  or when you want to know which MCP servers, remote targets, tunnels and memory project are
  here.
---

# swiss brief

Several agents often share this machine. Two builds at once, or a build beside a test suite,
starve each other: tests fail at their own deadlines and nobody's work finishes sooner. The
swiss gateway keeps one machine-wide **lane** for that work, and `swiss brief` says what else
the gateway offers right now.

Run `swiss brief` first: it lists the lane, the MCP servers, the remote targets, the managed
apps, the tunnels and this directory's memory project, each with the command that uses it.

## Heavy work goes through the lane

```
swiss lane run -- cargo test -j 2 -p mycrate
swiss lane run --wait 10m -- npm test
```

- It waits for a free slot, printing a line only when the queue moves, then runs the command
  **here**: this directory, this environment, its output on this terminal. It exits with the
  command's code.
- It adds the lane's caps (`CARGO_BUILD_JOBS`, `VITEST_MAX_WORKERS`) only where you set none.
  Keep your own explicit limits such as `-j 2` or `--maxWorkers=2`; they win.
- The last line names the full log (`~/.swiss/lane/...log`). Read that file instead of running
  the command again to see output you missed.
- A long build belongs in a background shell: start it, do other work, read the log when it ends.
  Waiting in the queue costs nothing; running beside another build costs both.
- `--wait DUR` gives up after DUR with exit 75 and runs nothing. Without it the command waits
  as long as the queue takes.
- Exit 127 means the program was not found; 130 means it was interrupted.
- One command per call. For a chain, wrap it: `swiss lane run -- sh -c "cargo build && cargo test"`.
- If the gateway is down or the lane is off, the command runs at once after one warning line.
  The lane is a courtesy, never a gate.

`swiss lane` shows who holds the lane and who waits. `swiss lane release <id>` frees a slot
whose holder is stuck; the command itself runs on, so use it only when the user asks.

Do not wrap a command that replaces or stops the swiss binary itself (a swiss deploy or update
script): it would hold the binary open while the script tries to replace it.

Light commands (`git`, `ls`, a single small test, a formatter) run directly. They do not need
the lane.

## Managed apps

```
swiss process status            # every app: status, pid, ports, uptime
swiss process start <name>      # also stop | restart; waits for the outcome
swiss process logs <name> -n 200
swiss process logs <name> -f    # follow
```

An app swiss manages is started, stopped and logged through it. Do not start a second copy by
hand next to it, and do not kill its pid. Adding an app is the user's job on the Process page.

## The rest of the brief

- **MCP servers**: a client connected to the gateway already has them as tools. `swiss connect
  claude|codex|cursor` points a client at it.
- **Remote targets**: use the `swiss-remote` skill, and only after the user asks for remote work.
- **Tunnels**: connect to the local port; the tunnel carries it to the far side.
- **Memory**: use the `swiss-memory` skill.

`swiss brief --json` gives the same facts as one object.
