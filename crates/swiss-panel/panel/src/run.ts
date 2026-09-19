/*
 * Copyright 2026 The swiss authors
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

import type { ApiMcpRow, ApiMcpTool, ToolSchemaProp } from "./types/api.js";
import type { PhantomMcpRow } from "./types/dom.js";
import type { McpDetail } from "./types/state.js";
import { $, esc } from "./util.js"; // esc: the string runBody twin, until pane converts
import { h } from "./h.js";
import type { HChild } from "./h.js";
import { histButtonLabel } from "./run-history.js";

/* --- Run: invoke a tool from the panel -------------------------------------------------------- */
/* argFieldsHtml, the STRING twin below, RETIRES with the jobs conversion (docs/37 R5 view
   9/11): the jobs editor still splices it into its sheet strings, and a node cannot ride
   through a concatenation. Run itself builds nodes - argFieldsNode - and reads them back
   with readRunArgs, which is DOM-shape agnostic (it reads by id). */
/** Argument inputs generated from the tool's own inputSchema, so this works for any MCP the gateway
 *  hosts — including a proc child whose tools the gateway knows nothing about. */
function argFieldsHtml(tool: ApiMcpTool, idPrefix?: string, values?: Record<string, unknown>): string {
  // idPrefix keeps the ids unique when the fields are embedded next to the Run view's own
  // (the jobs editor reuses this builder inside its sheet under a "ja-" prefix), and
  // `values` prefills them from an existing definition's action.input.
  const pfx = idPrefix || "r-arg-";
  const have = values || {};
  const schema = tool.inputSchema || {};
  const props: Record<string, ToolSchemaProp> = schema.properties || {};
  const required = schema.required || [];
  const keys = Object.keys(props);
  if (!keys.length) return '<div class="hint">This tool takes no arguments.</div>';
  return keys.map((k) => {
    const p = props[k] || {};
    const id = pfx + k;
    const kind = p.type === "array" ? "array"
      : p.type === "object" ? "object"
      : p.type === "boolean" ? "boolean"
      : (p.type === "number" || p.type === "integer") ? "number" : "string";
    // Not escaped with the key: the star is markup, so it is concatenated after esc(k), never into it.
    const star = required.indexOf(k) >= 0 ? ' <span class="req-star">*</span>' : "";
    const hint = p.description ? '<div class="hint">' + esc(p.description) + "</div>" : "";
    // esc() on the id too: the key comes from the tool's own inputSchema, and a proc MCP's child
    // controls that completely — an unescaped key like `x" autofocus onfocus="…` breaks out of the
    // attribute and runs on render. getElementById still matches, because the browser decodes the
    // entities back to the raw key when it parses the attribute.
    let attrs = ' id="' + esc(id) + '" data-arg="' + esc(k) + '" data-kind="' + kind + '"';
    if (kind === "boolean") {
      // Name and star in one span: .check is a flex row with an 8px gap, so a bare text node would
      // leave the star floating a gap away from the name it belongs to.
      const chk = have[k] ? " checked" : "";
      return '<div><label class="check"><input type="checkbox"' + attrs + chk + "><span>" + esc(k) + star +
        "</span></label>" + hint + "</div>";
    }
    // A constrained field renders as a dropdown rather than a free-text box that shows the allowed
    // values nowhere. The blank first option keeps "leave this argument out" reachable.
    if (Array.isArray(p.enum) && p.enum.length) {
      const opts = '<option value=""></option>' + p.enum?.map((v: unknown): string => {
        return '<option value="' + esc(v as string) + '"' + (have[k] === v ? " selected" : "") + '>' + esc(v as string) + "</option>";
      }).join("");
      return '<div><label class="field"><span>' + esc(k) + star + "  ·  " + esc(kind) + "</span>" +
        "<select" + attrs + ">" + opts + "</select></label>" + hint + "</div>";
    }
    const area = kind === "array" || kind === "object" || k === "sql";
    const itemType = kind === "array" && p.items && p.items.type ? String(p.items.type) : "";
    if (itemType) attrs += ' data-items="' + esc(itemType) + '"';
    const ph = k === "sql" ? "SELECT 1"
      : kind === "array" ? "one value per line" + (itemType ? " (" + itemType + ")" : "")
      : kind === "object" ? "{ }" : "";
    const prefilled = have[k] == null ? "" : kind === "object" || kind === "array" ? JSON.stringify(have[k], null, 1) : String(have[k]);
    const input = area
      ? "<textarea" + attrs + ' placeholder="' + esc(ph) + '">' + esc(prefilled) + "</textarea>"
      : '<input type="text"' + attrs + ' placeholder="' + esc(ph) + '" value="' + esc(prefilled) + '">';
    return '<div><label class="field"><span>' + esc(k) + star + "  ·  " + esc(kind) + "</span>" + input + "</label>" + hint + "</div>";
  }).join("");
}

function readRunArgs(tool: ApiMcpTool, idPrefix?: string): Record<string, unknown> {
  const pfx = idPrefix || "r-arg-";
  const out: Record<string, unknown> = {};
  const props = (tool && tool.inputSchema && tool.inputSchema.properties) || {};
  Object.keys(props).forEach((k) => {
    const node = $<HTMLInputElement>(pfx + k);
    if (!node) return;
    const kind = node.dataset.kind;
    if (kind === "boolean") { if (node.checked) out[k] = true; return; }
    const raw = node.value.trim();
    if (raw === "") return;
    if (kind === "number") { const n = Number(raw); if (!isNaN(n)) out[k] = n; return; }
    if (kind === "array") {
      // Coerce each line to the declared item type: a schema saying items are numbers and getting
      // ["1","2"] is rejected by any server that validates its input.
      const itemType = node.dataset.items || "";
      out[k] = raw.split(/\r?\n/).map((s) => { return s.trim(); }).filter(Boolean).map((s) => {
        if (itemType === "number" || itemType === "integer") { const n = Number(s); return isNaN(n) ? s : n; }
        if (itemType === "boolean") return s === "true" ? true : s === "false" ? false : s;
        return s;
      });
      return;
    }
    if (kind === "object") { try { out[k] = JSON.parse(raw); } catch (e) { throw new Error("`" + k + "` is not valid JSON"); } return; }
    out[k] = raw;
  });
  // A required argument left blank is refused HERE, with the field focused, instead of being
  // sent and coming back as "-32603: sql is required" from the far side. The red * already says
  // it is required; the form should act on that before the call leaves the browser.
  const required = (tool && tool.inputSchema && tool.inputSchema.required) || [];
  for (let i = 0; i < required.length; i++) {
    const rk = required[i];
    if (!(rk in props) || out[rk] !== undefined) continue;
    const missing = $(pfx + rk);
    if (missing && typeof missing.focus === "function") missing.focus();
    throw new Error("`" + rk + "` is required");
  }
  return out;
}

/** The node twin of argFieldsHtml (docs/37 R5): same ids, same data-arg/data-kind contract,
 *  same required star - but schema keys, descriptions and placeholders are text nodes and
 *  properties, so a hostile schema key from a proc child is a value, never markup. */
function argFieldsNode(tool: ApiMcpTool, idPrefix?: string, values?: Record<string, unknown>): HChild {
  const pfx = idPrefix || "r-arg-";
  const have = values || {};
  const schema = tool.inputSchema || {};
  const props: Record<string, ToolSchemaProp> = schema.properties || {};
  const required = schema.required || [];
  const keys = Object.keys(props);
  if (!keys.length) return h("div", { class: "hint" }, "This tool takes no arguments.");
  return keys.map((k) => {
    const p = props[k] || {};
    const id = pfx + k;
    const kind = p.type === "array" ? "array"
      : p.type === "object" ? "object"
      : p.type === "boolean" ? "boolean"
      : (p.type === "number" || p.type === "integer") ? "number" : "string";
    const star = required.indexOf(k) >= 0 ? h("span", { class: "req-star" }, "*") : null;
    const hint = p.description ? h("div", { class: "hint" }, p.description) : null;
    const data = { arg: k, kind: kind };
    if (kind === "boolean") {
      // Name and star in one span: .check is a flex row with an 8px gap, so a bare text node would
      // leave the star floating a gap away from the name it belongs to.
      return h("div", null,
        h("label", { class: "check" },
          h("input", { type: "checkbox", id: id, data: data, checked: have[k] === true }),
          h("span", null, k, " ", star)),
        hint);
    }
    // A constrained field renders as a dropdown rather than a free-text box that shows the allowed
    // values nowhere. The blank first option keeps "leave this argument out" reachable.
    if (Array.isArray(p.enum) && p.enum.length) {
      return h("div", null,
        h("label", { class: "field" },
          h("span", null, k, " ", star, "  ·  ", kind),
          h("select", { id: id, data: data },
            h("option", { value: "" }),
            p.enum.map((v: unknown) => { return h("option", { value: String(v), selected: have[k] === v }, String(v)); }))),
        hint);
    }
    const area = kind === "array" || kind === "object" || k === "sql";
    const itemType = kind === "array" && p.items && p.items.type ? String(p.items.type) : "";
    const dataAll = itemType ? Object.assign({}, data, { items: itemType }) : data;
    const ph = k === "sql" ? "SELECT 1"
      : kind === "array" ? "one value per line" + (itemType ? " (" + itemType + ")" : "")
      : kind === "object" ? "{ }" : "";
    const prefilled = have[k] == null ? "" : kind === "object" || kind === "array" ? JSON.stringify(have[k], null, 1) : String(have[k]);
    const input = area
      ? h("textarea", { id: id, data: dataAll, placeholder: ph }, prefilled)
      : h("input", { type: "text", id: id, data: dataAll, placeholder: ph, value: prefilled });
    return h("div", null,
      h("label", { class: "field" }, h("span", null, k, " ", star, "  ·  ", kind), input),
      hint);
  });
}

function runBodyNode(d: McpDetail, m: ApiMcpRow | PhantomMcpRow): HChild {
  if (m.lifecycle !== "started") {
    return h("div", { class: "group" },
      h("div", { class: "row" }, h("span", { class: "rowmsg" }, "Not started — start it to run a tool.")));
  }
  const kd = d.tools;
  if (kd.loading && !kd.loaded) return h("div", { class: "note" }, h("span", { class: "spin" }), " Loading tools…");
  if (kd.error) return h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg warn" }, kd.error)));
  const tools = kd.items || [];
  if (!tools.length) return h("div", { class: "group" },
    h("div", { class: "row" }, h("span", { class: "rowmsg" }, "This MCP exposes no tools.")));

  let current: ApiMcpTool | null = null;
  for (let i = 0; i < tools.length; i++) if (tools[i].name === d.run.tool) current = tools[i] as ApiMcpTool;
  if (!current) current = tools[0] as ApiMcpTool;
  d.run.tool = current.name;

  return h("div", { class: "group" },
    h("div", { class: "form" },
      h("label", { class: "field" },
        h("span", null, "Tool"),
        h("select", { id: "r-tool" },
          tools.map((t) => { return h("option", { value: t.name, selected: t.name === current?.name }, t.name); }))),
      current.description ? h("div", { class: "hint" }, current.description) : null,
      argFieldsNode(current),
      h("div", { class: "form-actions" },
        h("button", { class: "btn primary", id: "runBtn" }, "Run"),
        h("div", { class: "hist-wrap" },
          h("button", {
            type: "button",
            id: "r-hist",
            class: "hist-sel",
            // Same guard as renderHistoryOnly: a filter that matched nothing is a search in
            // progress, not an empty history - the control stays clickable so the popover
            // (and its empty state) can open.
            disabled: !d.run.histQ && d.run.histTool === d.run.tool && !(d.run.hist || []).length,
            aria: { haspopup: "true", expanded: d.run.histOpen ? "true" : "false" },
            title: "Fill the arguments from a past run — newest first, repeats shown once (up to 300). Type in the box to filter by arguments; hover an entry to read it in full. Secret values stay redacted.",
          }, histButtonLabel(d, current.name))),
        h("span", { class: "run-meta", id: "runMeta" })),
      h("pre", { class: "logs", id: "runOut" })));
}

export { argFieldsHtml, argFieldsNode, readRunArgs, runBodyNode };
