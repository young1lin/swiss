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

                                                                                             
                                                    
                                                  
import { $ } from "./util.js";
import { h } from "./h.js";
                                     
import { histButtonLabel } from "./run-history.js";
import { tr } from "./i18n.js";

/** What the argument-form generator needs of a tool: its input schema and nothing else, so
 *  an MCP tool and a jobs action (whose schema field is the same JSON Schema) both fit. */
                                                         

function readRunArgs(tool               , idPrefix         )                          {
  const pfx = idPrefix || "r-arg-";
  const out                          = {};
  const props = (tool && tool.inputSchema && tool.inputSchema.properties) || {};
  Object.keys(props).forEach((k) => {
    const node = $                  (pfx + k);
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
    if (kind === "object") { try { out[k] = JSON.parse(raw); } catch (e) { throw new Error(tr("run.kValidJson", { k })); } return; }
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
    throw new Error(tr("run.kRequired", { k: rk }));
  }
  return out;
}

/** The node twin of argFieldsHtml (docs/37 R5): same ids, same data-arg/data-kind contract,
 *  same required star - but schema keys, descriptions and placeholders are text nodes and
 *  properties, so a hostile schema key from a proc child is a value, never markup. */
function argFieldsNode(tool               , idPrefix         , values                          )         {
  const pfx = idPrefix || "r-arg-";
  const have = values || {};
  const schema = tool.inputSchema || {};
  const props                                 = schema.properties || {};
  const required = schema.required || [];
  const keys = Object.keys(props);
  if (!keys.length) return h("div", { class: "hint" }, tr("run.toolTakesArguments"));
  return keys.map((k) => {
    const p = props[k] || {};
    const id = pfx + k;
    const kind = p.type === "array" ? "array"
      : p.type === "object" ? "object"
      : p.type === "boolean" ? "boolean"
      : (p.type === "number" || p.type === "integer") ? "number" : "string";
    const star = required.includes(k) ? h("span", { class: "req-star" }, "*") : null;
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
            p.enum.map((v         ) => { return h("option", { value: String(v), selected: have[k] === v }, String(v)); }))),
        hint);
    }
    const area = kind === "array" || kind === "object" || k === "sql";
    const itemType = kind === "array" && p.items && p.items.type ? String(p.items.type) : "";
    const dataAll = itemType ? Object.assign({}, data, { items: itemType }) : data;
    const ph = k === "sql" ? tr("run.selectN1")
      : kind === "array" ? (itemType ? tr("run.oneValueLineType", { type: itemType }) : tr("run.oneValueLine"))
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

function runBodyNode(d           , m                           )         {
  if (m.lifecycle !== "started") {
    return h("div", { class: "group" },
      h("div", { class: "row" }, h("span", { class: "rowmsg" }, tr("run.startedStartRunTool"))));
  }
  const kd = d.tools;
  if (kd.loading && !kd.loaded) return h("div", { class: "note" }, h("span", { class: "spin" }), " ", tr("run.loadingTools"));
  if (kd.error) return h("div", { class: "group" }, h("div", { class: "row" }, h("span", { class: "rowmsg warn" }, kd.error)));
  const tools = kd.items || [];
  if (!tools.length) return h("div", { class: "group" },
    h("div", { class: "row" }, h("span", { class: "rowmsg" }, tr("run.mcpExposesTools"))));

  let current                    = null;
  for (let i = 0; i < tools.length; i++) if (tools[i].name === d.run.tool) current = tools[i]              ;
  if (!current) current = tools[0]              ;
  d.run.tool = current.name;

  return h("div", { class: "group" },
    h("div", { class: "form" },
      h("label", { class: "field" },
        h("span", null, tr("run.tool")),
        h("select", { id: "r-tool" },
          tools.map((t) => { return h("option", { value: t.name, selected: t.name === current?.name }, t.name); }))),
      current.description ? h("div", { class: "hint" }, current.description) : null,
      argFieldsNode(current),
      h("div", { class: "form-actions" },
        h("button", { class: "btn primary", id: "runBtn" }, tr("run.run")),
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
            title: tr("run.fillArgumentsPastRun"),
          }, histButtonLabel(d, current.name))),
        h("span", { class: "run-meta", id: "runMeta" })),
      h("pre", { class: "logs", id: "runOut" })));
}

export { argFieldsNode, readRunArgs, runBodyNode };