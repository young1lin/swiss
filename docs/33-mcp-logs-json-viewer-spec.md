# docs/33 — MCP Logs: JSON viewer for arguments and replies

Status: Draft — implementation in flight (C1, C2 below, one commit each, red-first tests).

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
- No new dependency, no web font, no second highlight engine. The SQL highlighter's
  token palette (views.css `.db-sql-hl`) is the colour precedent; JSON reuses its
  variables, not new colours.

## C1 — colour + copy, before any tree

- `hlJson(prettyText) -> html`: a hand tokenizer over the PRETTY text only. Tokens:
  object keys `.k` (--accent, 600), string values `.s` (--green), numbers `.n`
  (--amber), `true/false/null` `.b` (--text-3, 600). Punctuation unstyled. Text that
  does not parse as JSON never enters the highlighter — it stays exactly as arrived.
- Every token's text is escaped; the highlighter builds HTML only from tokens it split,
  so an adversarial payload can never inject markup.
- Both blocks (Arguments, Result) gain an icon `copy` button in their label row — a new
  `i-copy` sprite symbol (Lucide-style two-rect, 24 viewBox, 1.5 stroke). It copies the
  block's pretty text (the same bytes the panel shows). House clipboard idiom: async
  clipboard, execCommand fallback, toast `Copied`.

## C2 — the collapsible tree

- A block whose text parses as a JSON object or array renders as a tree instead of a
  highlighted pre. Everything else (plain strings, error text, truncated payloads that
  no longer parse) keeps the C1 pre — the tree is for structure, not a requirement.
- One node row: chevron button (containers only) · key or index · value. Leaf values
  carry their type colour from C1's palette. A collapsed container summarises its
  payload: `{ 3 keys }` / `[ 5 items ]`.
- Children render lazily on first expand. The default open depth is 2 — enough to read
  a call's shape at a glance; deeper levels are one click each. A node's expansion
  state lives in the detail (`d.callsTree[seq]`, keyed by a stable path string), so the
  6-second repaint on page 0 rebuilds the tree WITH the operator's open nodes intact.
- Per-node copy: every node row carries a hover/focus-revealed `copy` icon button. A
  container copies its whole value as pretty JSON; a leaf copies its scalar text
  verbatim (strings unquoted). Keyboard: chevrons and copy buttons are real buttons —
  Enter and Space work without extra wiring.
- Long leaf strings truncate for display at 200 characters with an ellipsis; the copy
  button still copies the full value.
- The existing `Show full result` fetch is unchanged: the preview's 2 KB head may not
  parse; once the full reply lands the tree replaces the truncated view.

## Acceptance

- vitest: highlighter token/escape cases; copy wiring per block; tree build from real
  payloads, lazy children, expansion persistence across a repaint, per-node copy
  payloads, non-JSON fallbacks. Red first.
- Live on 19998 (agent-browser, real pointer events): a redis call with nested
  arguments opens as a tree, levels collapse and expand, copy buttons answer with
  `Copied`, the poll repaint keeps open nodes, light and dark both legible, 900px
  does not overflow horizontally.
