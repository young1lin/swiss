# docs/33 — MCP Logs: JSON viewer for arguments and replies

Status: Shipped — C1 in e844b98, C2 in 26a448f, then refined after the live result exposed excessive density. The current viewer omits the redundant synthetic root, shows only the useful top level initially, folds nested containers, uses neutral text-face keys and compact inline controls, and keeps pretty-printing entirely in the panel. Direct-adapter tool results travel to the AI as compact JSON; display whitespace never consumes model context. The original live-found fixes remain covered (dataset-string seq mounting and preserving the slot's `jtree` class).

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

## C1 — initial colour + copy (colour superseded by C2)

- C1 introduced a hand-tokenized highlighted `<pre>`. C2 deleted it: parseable JSON now
  enters the structural tree, and text that cannot parse must stay byte-honest rather than
  being partly highlighted. The current tree uses the restrained key/value treatment below.
- Both blocks (Arguments, Result) retain C1's icon `copy` button in their label row and the
  `i-copy` sprite. It copies the formatted display text. House clipboard idiom: async
  clipboard, execCommand fallback, toast `Copied`.

## C2 — the collapsible tree

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

## Acceptance

- vitest: compact-wire display formatting and truncation-note preservation; copy wiring per
  block; tree build from real and empty payloads; lazy children; expansion persistence across
  a repaint; per-node copy payloads; non-JSON fallbacks. Red first.
- Live on 19998 (real pointer events): a nested call opens to its useful top-level fields,
  every nested container starts folded, levels expand and collapse, copy buttons answer with
  `Copied`, the poll repaint keeps open nodes, light and dark are legible, and 900px does not
  overflow horizontally.
- The direct-adapter serialization test pins the AI-facing text to compact JSON. The panel
  independently parses that compact text and owns all display formatting.
