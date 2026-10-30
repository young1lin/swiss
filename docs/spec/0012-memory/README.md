# §memory — Project memory for code agents

Chapter 0012 of the [swiss specification](../../SPEC.md).

A local, cross-agent, project-scoped **graph of notes** holding non-obvious, durable knowledge —
pitfalls, constraints, decisions, preferences and facts the code does not make obvious — put in
front of every code agent (Claude Code, Codex, Cursor, any MCP client, any agent with a shell)
when it is relevant, and grown by those agents. A project's notes are filed under a **map** of
its domains (§memory.domains) - the areas of the code, each with a hub saying what it is and
where it lives - so an agent reads the map first and the details where it works, never a bag of
fragments. Beside the notes a project keeps its documents whole (§memory.docs) - design docs,
plans, pitfall logs - each carded with the code it is about. One engine and one rule set behind every entry
point: the offline CLI, one MCP endpoint and the panel (ADR-031: SQLite+FTS5 under the home, no
per-project databases or endpoints). The gateway's **process memory (RSS)** is a different thing
(§product.memory, `GET /api/memory`) that this plugin never touches. The full design rationale is
`bin/SWISS_MEMORY_SPEC.md` (v2.1); this chapter is the contract.

**Crate.** `swiss-memory` owns model, scope resolution, store, search, association, graph, brief,
freshness, transfer, the text renderings, and sync's transcript digest, prompt, proposal and
apply; it depends on `swiss-core` only — no axum, no rmcp, no peer crate, no child process. The
adapters live in the root's `src/memory_cli/` and in `crates/swiss-plugins/src/` (`memory.rs`,
`memory_mcp.rs`, `memory_api.rs`, `memory_sync/`, `memory_agent.rs`, `memory_embed.rs`,
`memory_preview.rs`) and hold no
business rules: every rule lives in `MemoryService`. The code index (§memory.code) lives in the
crate too, beside the store and apart from it. The models are the llm plugin's (§llm): the
adapters read its shared state - the embedding block, the agent a sync run names - and
`swiss-memory` never links `swiss-llm`.

**Parts.** The sections live in six files beside this one, in reading order:

1. [The record](01-record.md) — notes and their lifecycle, projects, the write path, the project map, freshness, storage
2. [Reading it back](02-reading.md) — the brief, search, semantic search, the graph, asking a question
3. [Code and documents](03-sources.md) — the code index, the documents a project keeps, previews in place
4. [Initialization](04-init.md) — the first map drawn from the code, full knowledge initialization
5. [Following the code](05-sync.md) — sync, and which agents a run asks
6. [Entry points and trust](06-surfaces.md) — the CLI, the MCP endpoint, the API, the panel, logs and trust, verification
