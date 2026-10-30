/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

import type { ApiMcpRow } from "./types/api.js"; // Inventory contract.
import { h } from "./h.js";
import { tr } from "./i18n.js";
import { btn, card, dot, note, paneBody, paneHead, row, section } from "./ui/index.js";
import type { DotState } from "./ui/index.js"; // Shared status marks.

/** A useful first frame without making a tools/list request or waking a lazy process. */
export function mcpOverview(rows: ApiMcpRow[], open: (name: string) => void, add: () => void): HTMLElement {
  const addButton = btn(tr("pane.addMcp"), { kind: "primary", icon: "plus" });
  addButton.onclick = add;
  const content = rows.slice(0, 6).map((server) => {
    const action = btn(tr("desktop.open"));
    action.onclick = () => { open(server.name); };
    const state = dot(server.state as DotState, server.state);
    state.setAttribute("data-overview-state", "");
    return row({
      name: server.name,
      sub: server.description || server.type,
      lead: state,
      data: { "overview-server": server.name },
      cols: [h("code", null, server.type)],
      primary: action,
    });
  });
  const view = paneBody({},
    paneHead({ title: tr("desktop.workspace"), desc: tr("desktop.workspaceHint"), actions: [addButton] }),
    section({ cap: tr("desktop.servers") }, card(...content)),
    note(tr("desktop.sidebarHint")));
  view.id = "mcpOverview";
  return view;
}

/** A poll updates the state without replacing a focused Open button. The sidebar remains
 *  the complete, live inventory; the landing page's entry list refreshes on navigation. */
export function patchMcpOverview(rows: ApiMcpRow[]): boolean {
  const view = document.getElementById("mcpOverview");
  if (!view || rows.length === 0) return false;
  const current = new Map(rows.map(server => [server.name, server]));
  view.querySelectorAll<HTMLElement>("[data-overview-server]").forEach(node => {
    const server = current.get(node.getAttribute("data-overview-server") || "");
    const button = node.querySelector("button");
    if (button) button.disabled = !server;
    const mark = node.querySelector("[data-overview-state]");
    if (server && mark) {
      const next = dot(server.state as DotState, server.state);
      next.setAttribute("data-overview-state", "");
      mark.replaceWith(next);
    }
  });
  return true;
}
