---
name: swiss
description: swiss remote - run commands and move files on SSH machines via the local gateway. Invoke only when the user explicitly asks.
disable-model-invocation: true
---

# swiss remote

Run commands on the machines the gateway's Tunnels plugin already reaches. Targets name a
tunnels connection id plus an absolute workspace path - never a host, user or password.

```bash
swiss remote endpoints                                  # what the transport serves
swiss remote target add build --endpoint conn-1 --root /data/ws/proj --caps exec,sync
swiss remote exec build -- make -j8                     # streams, exits with the REMOTE exit code
swiss remote exec build --timeout 30m -- ./test.sh -k   # everything after -- is ARGV, untouched
swiss remote exec build --cwd /home/dev/app -- ls  # ABSOLUTE paths pass as-is (ssh trust); relative ones resolve under the root
swiss remote sync build                                 # upload a tree (never deletes); .git/ target/ excluded
swiss remote push build app.exe                         # upload one file
swiss remote cat build config.toml                      # print a remote file to stdout
swiss remote write build config.toml < config.toml      # stdin becomes the remote file (overwrite)
swiss remote pull build out/app.bin --to artifacts/app.bin
swiss remote pull build out/dists                       # a directory pulls recursively
swiss run logs 17 -f; swiss run cancel 17               # detached runs: swiss remote exec ... --detach
```

A repository can carry `.swiss/remote.json` (plain JSON, no secrets) naming targets and
actions like `build`; `swiss remote exec build -- make` then resolves through it.
workspaceRoot is a guardrail, not a sandbox - the command runs as the SSH login user.
sudo passes through like any command (no PTY, so it needs NOPASSWD or -n). Full contract: docs/32.

Everything else the gateway does (MCPs, databases, jobs, panel): ask the user, or check
`swiss --help` and the panel at http://127.0.0.1:19999.
