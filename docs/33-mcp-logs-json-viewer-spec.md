# docs/33 — MCP Logs: JSON viewer for arguments and replies

Status: Shipped — C1 in e844b98, C2 in 26a448f, **C3 (2026-09-23) supersedes the C2 tree**: the operator found that reading a reply took a click per level and that the tree itself was uncomfortable to read, so a call's arguments and reply are now one formatted JSON code block, the same for every MCP. Direct-adapter tool results still travel to the AI as compact JSON; all display formatting stays in the panel and never consumes model context.

**Revised by docs/46 §3.2 (P2-2, 2026-09-23):** two C3 rules changed when Logs became the library's
event list. (1) A value whose compact JSON is at most 80 characters, with no string that breaks
lines, prints on ONE line (`{"sql": "SELECT 1"}`) instead of always indented; longer values, and
JSON followed by prose, keep the indented block. (2) Each block has ONE visible copy button (Copy:
the decoded structure as valid JSON); Copy raw moved behind the block's ⋯. What each copy writes
is unchanged. A call's body is also painted on open only, no longer on every closed row.

**Closed by docs/46 P9 (2026-09-24):** the P2-2 rules above are the final shape. Logs, Traffic and
run history all paint on the library's one event list (`ui/timeline.ts`), and the P9 ratchet sweep
left `views.css` with zero rules restyling a library class — nothing in this spec still owns CSS.

The Logs tab (docs/31 search, docs/32 paging) ships tool arguments and replies as flat
pretty-printed `<pre>` text. Three gaps, reported by the operator on the live panel:

1. No syntax colouring — a 2 KB single object is a wall of same-colour text.
2. No copy affordance — the arguments JSON cannot be copied out of the panel at all.
3. No structure — nested objects and arrays cannot be walked level by level; a deep
   value's content is only visible by reading the flat text.

## Non-goals

- No editing, no diffing, no external viewer link. The log is a record, not an editor.
- No streaming/virtualised tree: a log page is 20 rows and only OPEN rows render bodies;
  lazy child rendering keeps the built DOM proportional to what the operator opened.
- No new dependency, no web font, no second highlight engine. (C1/C2 reused the SQL
  highlighter's variables; C3 adds five muted `--syn-*` tokens, see C3 below.)
- Nothing MCP-specific. The view must read the same for mysql, redis, pg, search, figma
  and any MCP added later — no SQL- or redis-shaped special cases (the operator rejected
  an SQL-first design for exactly this reason).

## C1 — initial colour + copy (colour superseded by C2)

- C1 introduced a hand-tokenized highlighted `<pre>`. C2 deleted it: parseable JSON now
  enters the structural tree, and text that cannot parse must stay byte-honest rather than
  being partly highlighted. The current tree uses the restrained key/value treatment below.
- Both blocks (Arguments, Result) retain C1's icon `copy` button in their label row and the
  `i-copy` sprite. It copies the formatted display text. House clipboard idiom: async
  clipboard, execCommand fallback, toast `Copied`.

## C2 — the collapsible tree (superseded by C3)

- A block whose text parses as a JSON object or array renders as a tree instead of a
  highlighted pre. Everything else (plain strings, error text, truncated payloads that
  no longer parse) keeps the C1 pre — the tree is for structure, not a requirement.
- The block label already identifies the JSON root, so the tree does not paint a second
  synthetic root row. Its immediate fields appear directly; every nested object or array
  starts folded and summarises itself as `{3}` / `[5]`.
- Keys are field identity: neutral, sans-serif, and unquoted in the inspector. Values are
  copyable data and remain monospace. Syntax colour does not turn a large result into a
  field of saturated green and blue.
- Children render lazily on first expand. A node's expansion state lives in the detail
  (`d.callsTree[seq]`, keyed by a stable path string), so the 6-second repaint on page 0
  rebuilds the tree with the operator's open nodes intact.
- Per-node copy: every node row carries a compact hover/focus-revealed `copy` icon button.
  A container copies its whole value as pretty JSON; a leaf copies its scalar text verbatim
  (strings unquoted). Keyboard: chevrons and copy buttons are real labelled buttons — Enter
  and Space work without extra wiring.
- Long leaf strings truncate for display at 200 characters with an ellipsis; the copy
  button still copies the full value.
- The existing `Show full result` fetch is unchanged: the preview's 2 KB head may not
  parse; once the full reply lands the tree replaces the truncated view.

## C3 — one formatted code block (supersedes C2)

Why: the C2 tree folded every level, so seeing what a reply said took several clicks; its
sans-serif unquoted keys beside quoted mono values read as neither JSON nor prose; replies
that are JSON inside a string (web_search_prime, webReader, redis hash values) or JSON
followed by prose (figma) did not parse at all and fell back to one long escaped line; and
a 360px inner scroll box sat inside the page's own scroll. The operator's ask was plain:
format the JSON input and output, and make it easy to copy.

- **One block per side.** Arguments and Result each render as a single `<pre class="jv">`:
  standard JSON, two-space indent, every line visible, no folding (json-view.ts
  `jsonCodeNode`). Its text IS `JSON.stringify(v, null, 2)` for a plain value, so a
  drag-selection copies valid, indented JSON.
- **JSON in a string is shown as JSON.** A string whose text is an object, array or JSON
  string literal is parsed — as many layers as the wire stacked — and printed as the
  structure it holds behind a `decoded` / `decoded ×N` marker, so the display never claims a
  shape the wire did not have. `"50"` and `"true"` stay strings (only `{`, `[` and `"`
  qualify). The label row says `JSON decoded from a string`.
- **JSON + prose.** A leading object/array followed by text keeps the JSON formatted and the
  text in a plain block below (label note `JSON + text`) — also the gateway's own
  `[showing the first N of M …]` note.
- **Everything else is honest.** Text that is not JSON, markdown, a preview clipped
  mid-value, and every error reply stay exactly as they arrived (errors in red).
- **Real line breaks.** A string's `
` / `
` print as real breaks so a SQL statement or a
  message keeps its shape; every other escape stays JSON-escaped.
- **Two copies per block, always visible** (copying is the frequent act here, not a hover
  discovery): `Copy` = valid indented JSON of the decoded structure (plus the prose after
  it); `Copy raw` = the stored text byte for byte. Both read the call row, not the DOM, so a
  block cut at its line cap still copies everything. The decoded marker's words live in
  `::before`, so no selection picks them up.
- **No inner scroll box.** The page scrolls. A block past 200 lines (`JV_LINES`) stops
  building nodes and ends in `Show all N lines`; the choice is state (`d.callsAll`), so the
  6-second repaint keeps it. The cap also bounds the DOM a 1 MB stored body could build.
- **Opening a clipped row fetches it whole.** The page carries a 2 KB preview of each reply;
  opening the row (click or Enter/Space) fetches the full body once — an open/close/open does
  not stack requests — and repaints only that block. `Show full result` stays as the retry.
  A pruned body (`bodyGone`) is recorded (`d.callsGone`) and said in place under the
  preview instead of by toast; the row stops offering the fetch.
- **Colour is information here.** Keys, strings, numbers, literals and punctuation take five
  muted `--syn-*` tokens (light and dark in base.css) — the one documented exception to
  design rule 2 (swiss-ui-design §16), because in a code block the hue says which token a
  character belongs to.

| Feature point | Unit (vitest) | Integration | Rationale comment |
| --- | --- | --- | --- |
| formatted block = `JSON.stringify(v,null,2)`, token colours, real line breaks | admin-logs-json-viewer `jsonCodeNode` suite | real-module-graph paint in the same suite (`callNode` → DOM) | json-view.ts header, `stringLiteral` |
| JSON-in-string decoding, multi-layer, `"50"` stays text | `decodeStrings` suite | swiss-it `proc::the_call_log_keeps_replies_verbatim_for_the_logs_view` (the log stores the wrapped reply verbatim) | json-view.ts `parseJsonText`, `decodeStrings` |
| JSON + prose split; text/errors untouched | `splitJsonBlock` suite, error/empty paint test | — (pure display) | json-view.ts `splitJsonBlock`, logs.ts `callBlockNode` |
| Copy formatted / Copy raw | `formattedCopyText`, `callBlockCopyText` | admin-logs-pagination "the block buttons dispatch" (real dispatcher) + swiss-it verbatim storage | run-history.ts dispatcher, logs.ts `blockText` |
| 200-line cap + Show all | cap tests in both suites | admin-logs-pagination Show all dispatch | json-view.ts `JV_LINES`, run-history.ts dispatcher |
| fetch on open, once; pruned said in place | clipped/pruned paint test | admin-logs-pagination "opening a clipped call fetches its full reply" (5) + swiss-it (preview on the page, whole on `calls/{seq}`) | detail.ts `showFullResult`, run-history.ts `openOrCloseCall` |

## Acceptance

- vitest (C3): the table above; the 22 new json-viewer tests and the 7 new pagination tests
  were run red against the C2 code first.
- swiss-it gate 2: `proc::the_call_log_keeps_replies_verbatim_for_the_logs_view`.
- Live on a test instance (real pointer events, 2026-09-23): a synthetic stdio MCP returning
  every shape (JSON-in-string, redis hash with a twice-encoded value, JSON + prose, a 369-line
  reply over 2 KB, markdown, an error) plus a real mysql reply: each renders as above; one
  click on a clipped row fetched it whole; Show all, Copy and Copy raw produced the expected
  clipboard text; a mouse drag across a decoded block selected the JSON without the marker;
  zh and dark mode checked.
- The direct-adapter serialization test pins the AI-facing text to compact JSON. The panel
  independently parses that compact text and owns all display formatting.
