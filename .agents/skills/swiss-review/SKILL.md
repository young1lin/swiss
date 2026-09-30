---
name: swiss-review
description: Use after the cargo gates are green and before committing or deploying a swiss change — audit the diff, branch, or working tree against this repo's load-bearing rules, your own change or another agent's.
---

# Reviewing a swiss change

Guidance, not a checklist to tick blindly: read the diff, then enough surrounding code to understand
the design, then audit it against the load-bearing rules. One substantiated blocker beats ten nits.

Start with the mechanical half — `scripts/audit-diff.ps1` in this skill greps the outgoing diff
for the textual red flags below (never a gate, always a lens):

```powershell
& .agents\skills\swiss-review\scripts\audit-diff.ps1              # vs HEAD (staged + unstaged)
& .agents\skills\swiss-review\scripts\audit-diff.ps1 -Base main   # vs a branch
```

```bash
bash .agents/skills/swiss-review/scripts/audit-diff.sh              # vs HEAD (staged + unstaged)
bash .agents/skills/swiss-review/scripts/audit-diff.sh --base main  # vs a branch
```

## Map the diff first

```powershell
git status --short --branch
git diff --stat <base>        # and read the full diff against the correct base
```

Identify which surfaces the diff reaches (core / host / a subsystem crate / panel / scripts / docs)
— that decides which rules below actually apply and which evidence
[swiss-verify](../swiss-verify/SKILL.md) should select.

## The load-bearing audit — a break is a blocker

Each rule is a security or correctness boundary, not style. Verify by reading, not by assuming:

| Rule | What to check |
| --- | --- |
| Loopback-only is security | New routes bind/refuse correctly; non-loopback `host` still refused at config load, not warned about; no "reach it remotely" weakening |
| `/api/*` needs the admin session | No exemption for an `/api` path; the MCP bearer is never accepted there; the CLI key is never printed (SPEC §host.session) |
| Credentials are references, never literals | Values enter as `${ENV_VAR}` or `${secret://name}` (vault, SPEC §host.vault) refs; no literal secrets in `gateway.config.json`/`managed.json`/`tunnels.json` paths or code; panel still masks them back out |
| No health ping on `http`/`rest` adapters | Registry reports them "unknown" on purpose; no ping added "for completeness" |
| `proc` MCP stays lazy | Idle at boot, wake on first request, reap when idle — nothing new starts eagerly |
| Sealed envelope frozen | Any diff under `crates/swiss-core/src/secure/` altering envelope construction is a stop-and-discuss (SPEC §formats; the committed fixture under `tests/`) |
| `&RawValue` on forwarding paths | `proc`/`http`/`rest` adapters parse only envelope fields; no `serde_json::Value` materialisation of payloads |
| Runtime stays `current_thread` | No `multi_thread` without a measured reason in the commit message |
| No `unsafe` outside the platform FFI | `unsafe` belongs at the OS FFI boundary (`platform/`), nowhere else |
| No subprocess where a syscall exists | No new `Command::new("powershell")` (or equivalent) without a very good excuse |
| No `.unwrap()` on config/net/db/fs | One failing MCP must never take down the others |
| Panel change discipline | Panel sources are `crates/swiss-panel/panel/src/*.ts` with the emit committed under `src/admin_assets/js` (ADR-024); every panel change ships its vitest case in `crates/swiss-panel/panel/test/` and a fresh emit; `/api/*` response shapes unchanged — a shape change ships on both sides in one commit |
| Plugin contract respected | New capability contributed a descriptor/action/page; no new match arm in the host; no peer-to-peer crate edge ([swiss-add-plugin](../swiss-add-plugin/SKILL.md)) |
| Dependency weight | Manifest changes carry the justification and minimal features; `cargo tree -d` judged per [swiss-dependency-review](../swiss-dependency-review/SKILL.md) |

Also standard hygiene: LF everywhere, code comments in English, no secrets or `*.log`/
`master.key`/state files staged (including `~/.swiss/terminal/*.cast` recordings, which
echo typed passwords), no unrelated refactors bundled — and commit messages carry the
justifications this repo requires in them (dependency weight; any measured reason for
`multi_thread`).

## Fresh eyes and stale memory

- Self-review skims what its author already believes. When subagents are available, run this audit
  in one — a fresh window reading the diff without the author's reasoning catches what self-review
  rationalizes away — and for a diff over ~300 lines or one touching `swiss-host`,
  `crates/swiss-core/src/secure/`, or the panel, make it the default. Apply its findings back in
  the original context, where the change's reasoning still lives.
- Facts this repo memorizes must not be left stale: if the diff adds/removes tests or modules,
  moves files, or changes behavior that `docs/SPEC.md` or AGENTS.md describe, update those lines in the
  same change or say explicitly they were left stale. Never copy test counts into prose.

## Evidence and reporting

- Every behavior change ships with a test that fails before and passes after — verify the test
  actually fails on the pre-change code (RED), not merely passes after.
- Skipped DB tests are named, not hidden.
- Report blockers separately from suggestions, each with location, impact, and the evidence that
  shows it. Omit anything a green gate already enforces.
- Before commit: gates selected per [swiss-verify](../swiss-verify/SKILL.md); user-visible changes
  additionally pass [swiss-live-verify](../swiss-live-verify/SKILL.md). Deployment itself is
  [swiss-deploy](../swiss-deploy/SKILL.md) territory and only ever on request.
