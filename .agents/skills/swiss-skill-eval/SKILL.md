---
name: swiss-skill-eval
description: Pressure-test the swiss skill suite — run baseline-vs-treatment scenarios with fresh subagents and grade the behavior difference. Invoke after writing or significantly editing any skill, or when a skill seems to be ignored.
disable-model-invocation: true
---

# Evaluating the skill suite — RED-GREEN for skills

A skill you have never watched an agent fail WITHOUT is unproven. Opinions select the
hypothesis; only observed behavior confirms it. Two test families, both run with fresh
subagents so nobody grades their own writing:

1. **Pressure tests (behavior, A/B)** — the skill must change what an agent DOES under
   temptation. Baseline arm vs treatment arm, same scenario, graded against written markers.
2. **Trigger tests (recall, single arm)** — a fresh agent seeing only the catalog must pick
   the right skill for a raw user request.

The scenario bank with PASS/FAIL markers and every recorded result lives in
[scenarios.md](references/scenarios.md); append results there, never edit old ones.

## Pressure protocol (validated in round 1-2 — do not deviate without a reason)

**The trap that invalidates naive baselines:** subagents share the SESSION-level skill
catalog. A skills-free worktree hides the files, not the catalog — round-1 "baselines"
self-loaded skills anyway and had to be thrown away. Windows adds a second trap: renaming
the `.agents/skills` DIRECTORY is denied (the live watcher holds its handle). What works:

1. `worktree_enter` a fresh worktree — it isolates the repo tree (clean checkout, no
   gitignored state files, agents can read the real code) but it is NOT the baseline itself.
2. **Hide the suite at the file level**: in the MAIN checkout rename every model-visible
   `SKILL.md` to `SKILL.hidden` (one `pwsh` loop). Catalog updates live.
3. **Probe before launching**: one throwaway subagent — "list every skill in your catalog,
   names only". It must NOT list any swiss skill. Only then launch the baseline arm.
4. Baseline arm: scenario + planning-only guard + "no agent skill suite is installed".
   Keep the suite hidden for the arm's ENTIRE runtime; interrupt and relaunch if the probe
   was skipped.
5. Restore (`SKILL.hidden` → `SKILL.md`), confirm the catalog repopulated, then launch the
   treatment arm: identical scenario, suite visible, agents load skills on their own.
6. Grade both arms with the scenario's markers:
   - baseline fails AND treatment passes → the skill earns its place (GREEN);
   - treatment still fails → the body is too weak — fix the text, not the agent;
   - baseline passes → see the caveat below BEFORE trimming anything.

## The planning-mode caveat (learned in round 1-2, 2026-09-15)

With a strong model, fresh context, and planning-only constraints, all three true baselines
passed — AGENTS.md alone produced the discipline. A GREEN baseline there does NOT retire a
skill, because the round measured neither execution mode (real red test + deadline, where
rationalization lives), long sessions (where AGENTS.md recedes from attention), nor weaker
models. What planning-mode GREEN baselines DO license: cutting prose that merely restates
AGENTS.md, and keeping what only the skill carries — trigger routing, red-flag tables,
operational compression (exact commands, pins, scripts), and the scripts themselves.

## Trigger protocol

Fresh subagent, main repo, suite visible: "a user sends '<request>'; name the skills you
would invoke from the session catalog (exact names, or 'none'), one line of reasoning
each, then STOP — do not start the task or read the bodies." A miss or a collision is a
description bug: fix the frontmatter, never the scenario.

## Rules

- Planning-only inside pressure tests: no command execution, no file writes — an eval never
  touches port 19999, the daemon, or any working tree. (S6, execution mode, is the planned
  exception and its containment is specified in the bank.)
- One scenario per subagent; a subagent never sees another's answers.
- Every skill change ships with an eval round for the behavior it claims to change — the
  same test-first rule the repo applies to code.
