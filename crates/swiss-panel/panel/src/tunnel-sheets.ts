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

import type { ApiTunnelConnectionRow, ApiTunnelRuleMutation, ApiTunnelRuleRow, ApiTunnelsBrowseEntry, ApiTunnelsBrowseResponse, ApiTunnelsKeysResponse, ApiTunnelsSuggestResponse, ConnSheetDraft, RuleSheetDraft } from "./types/api.js";
import { $, api, apiJson, el, esc, state, toast } from "./util.js";
import { closeSheet } from "./add-sheet.js";
import { loadTunnels, tunConnName, tunData, tunGroupsList } from "./polling.js";
import { assignTunScoped } from "./tunnels.js";
import { groupFieldHtml, lastGroup, rememberGroup, resolveDefaultGroup } from "./groups.js";

/* --- connection sheet -------------------------------------------------------------------------- */

async function loadKeys(): Promise<ApiTunnelsKeysResponse> {
  if (state.tun.keys) return state.tun.keys;
  const j = await apiJson<ApiTunnelsKeysResponse>("/api/tunnels/keys");
  state.tun.keys = j || { keys: [], defaultPath: "" };
  return state.tun.keys!;
}

/** docs/27 §4: the Advanced fold — proxy and jump live here, collapsed by default. The
 *  summary chips keep "goes through a proxy / via <jump>" visible without unfolding, so
 *  collapsed never means hidden; with nothing configured the fold is the sheet's only new
 *  line. Native details/summary is the house fold primitive (the data-value JSON tree) —
 *  no open attribute, so the browser starts it folded. The jump dropdown lists every other
 *  connection (this one excluded); the backend stays the single source of truth for cycles,
 *  its 400 lands inline in the sheet, and the panel does not pre-walk chains. */
function advancedConnHtml(d: ConnSheetDraft, editing: boolean): string {
  let chips = "";
  if (d.proxy) chips += ' <span class="tag">proxy</span>';
  if (d.jump) chips += ' <span class="tag">via ' + esc(tunConnName(d.jump)) + "</span>";
  const opts = '<option value="">None</option>' + tunData().connections
    .filter((c: ApiTunnelConnectionRow): boolean => { return !editing || c.id !== d.id; })
    .map((c: ApiTunnelConnectionRow): string => {
      return '<option value="' + esc(c.id) + '"' + (d.jump === c.id ? " selected" : "") + ">" + esc(c.name) + "</option>";
    })
    .join("");
  return '<details class="fold" id="c-advanced">' +
      "<summary>Advanced" + chips + "</summary>" +
      '<div class="fold-body">' +
        '<div class="cap">Proxy</div>' +
        '<label class="field"><span>Proxy URL</span><input id="c-proxy" value="' + esc(d.proxy || "") +
          '" placeholder="socks5://127.0.0.1:7890" autocomplete="off" spellcheck="false"></label>' +
        '<div class="two">' +
          '<label class="field"><span>Proxy username</span><input id="c-proxy-user" value="' + esc(d.proxyUsername || "") +
            '" autocomplete="off"></label>' +
          '<label class="field"><span>Proxy password</span><input id="c-proxy-pass" type="password" value="' +
            esc(d.proxyPassword || "") + '" autocomplete="off"></label>' +
        "</div>" +
        '<div class="hint">Leave empty to connect directly. Credentials never go inside the URL — use the two fields above.</div>' +
        '<div class="cap">Via connection (jump)</div>' +
        '<label class="field"><span>Connection</span><select id="c-jump">' + opts + "</select></label>" +
        '<div class="hint">Dials through the chosen connection before reaching this host (OpenSSH -J). A proxy or a jump, not both — cycles are refused on save.</div>' +
      "</div>" +
    "</details>" +
    '<div class="hint" id="c-err" hidden></div>';
}

function openConnSheet(def: ApiTunnelConnectionRow | null): void {
  const editing = !!def;
  const d: ConnSheetDraft = def || { name: "", host: "", port: 22, username: "", authType: "key", keyPath: "", passphrase: "", password: "" };
  // Creating: a Group select names where the row lands (the header +'s group preselected, the
  // last-used one when the top New opened this). Editing: no field - moving a connection is
  // the list's gesture (drag / row menu), not a property of its definition.
  const names = tunGroupsList();
  const initial = state.tun.pendingGroup || resolveDefaultGroup(names, lastGroup("conns"));
  state.tun.pendingGroup = null; // consumed: the select is the truth from here
  const groupField = editing ? "" : groupFieldHtml(names, initial);
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit connection" : "New connection") + '">' +
      '<div class="sheet-head"><h2 id="t-title">' + (editing ? "Edit connection" : "New SSH connection in " + esc(initial)) + "</h2></div>" +
      '<div class="sheet-body">' +
        '<label class="field"><span>Name</span><input id="c-name" value="' + esc(d.name) + '" placeholder="Test server" autocomplete="off"></label>' +
        groupField +
        '<div class="two">' +
          '<label class="field"><span>Host</span><input id="c-host" value="' + esc(d.host) + '" placeholder="e.g. 192.168.1.100" autocomplete="off"></label>' +
          '<label class="field"><span>Port</span><input id="c-port" value="' + esc(String(d.port || 22)) + '" autocomplete="off"></label>' +
        "</div>" +
        '<label class="field"><span>Username</span><input id="c-user" value="' + esc(d.username) + '" placeholder="Enter a username" autocomplete="off"></label>' +
        '<label class="field"><span>Authentication</span><select id="c-auth">' +
          '<option value="key"' + (d.authType === "key" ? " selected" : "") + ">key — private key file</option>" +
          '<option value="password"' + (d.authType === "password" ? " selected" : "") + ">password</option>" +
        "</select></label>" +
        '<div id="c-auth-fields"></div>' +
        advancedConnHtml(d, editing) +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="c-cancel">Cancel</button>' +
        '<button class="btn primary" id="c-save">Save</button></div>' +
    "</div>";
  $("sheet").hidden = false;

  const paint = async (): Promise<void> => {
    const keys = await loadKeys();
    const isKey = $<HTMLSelectElement>("c-auth").value === "key";
    $("c-auth-fields").innerHTML = isKey
      ? '<div class="with-btn">' +
          '<label class="field"><span>Private key path</span><input id="c-keypath" value="' + esc(d.keyPath || "") +
            '" placeholder="' + esc(keys.defaultPath) + '" autocomplete="off" spellcheck="false"></label>' +
          '<button class="btn" id="c-browse">Browse…</button>' +
        "</div>" +
        '<label class="field"><span>Passphrase (optional)</span>' +
          '<input id="c-pass" type="password" value="' + esc(d.passphrase || "") + '" placeholder="Leave empty if the key has none" autocomplete="off"></label>' +
        '<div class="hint">Defaults to <code>' + esc(keys.defaultPath) + "</code> when left empty.</div>"
      : '<label class="field"><span>Password</span>' +
          '<input id="c-pass" type="password" value="' + esc(d.password || "") + '" placeholder="Enter the password" autocomplete="off"></label>';
    if ($("c-browse")) $<HTMLButtonElement>("c-browse").onclick = openKeyPicker;
  };
  void paint();
  $<HTMLSelectElement>("c-auth").onchange = (): void => { void paint(); };
  if ($("g-sel")) $<HTMLSelectElement>("g-sel").onchange = (): void => {
    $("t-title").textContent = "New SSH connection in " + $<HTMLSelectElement>("g-sel").value;
  };
  $<HTMLButtonElement>("c-cancel").onclick = closeSheet;
  $<HTMLButtonElement>("c-save").onclick = (): void => { void saveConn(def!); };
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };
  $<HTMLInputElement>("c-name").focus();
}

/**
 * A server-side directory browser for the private key. A browser file picker can't return a real
 * server-side path (only a bare filename), so Browse is served from /api/tunnels/browse, which can
 * reach any folder the gateway process can read — not just ~/.ssh. Folders navigate; files pick.
 */
async function openKeyPicker(): Promise<void> {
  const target = $<HTMLInputElement>("c-keypath");
  // Start in the current key's folder if one is set, otherwise let the backend default to ~/.ssh.
  let cur = target.value && target.value.trim() ? target.value.trim().replace(/[/\\][^/\\]*$/, "") : "";

  const back = el("div", "backdrop");
  back.style.zIndex = "60";
  const picker = el("div", "sheet");
  picker.setAttribute("role", "dialog");
  picker.setAttribute("aria-modal", "true");
  picker.setAttribute("aria-label", "Choose a private key file");
  back.appendChild(picker);
  document.body.appendChild(back);
  const close = (): void => { document.body.removeChild(back); target.focus(); };
  back.onclick = (e: MouseEvent): void => { if (e.target === back) close(); };

  async function render(): Promise<void> {
    const j = await apiJson<ApiTunnelsBrowseResponse>("/api/tunnels/browse" + (cur ? "?dir=" + encodeURIComponent(cur) : ""));
    if (!j) { toast("Could not read that folder", true); return; }
    cur = j.dir; // normalize to the resolved path the server returned
    const dirs = j.entries.filter((e: ApiTunnelsBrowseEntry): boolean => { return e.dir; });
    const files = j.entries.filter((e: ApiTunnelsBrowseEntry): boolean => { return !e.dir; });
    picker.innerHTML =
      '<div class="sheet-head"><h2>Choose a private key</h2></div>' +
      '<div class="sheet-body">' +
        '<div class="browse-cwd" style="display:flex;gap:8px;align-items:center;margin-bottom:8px">' +
          '<button class="btn' + (j.parent ? "" : " disabled") + '" id="b-up"' + (j.parent ? "" : " disabled") + ' title="Up one level">↑ Up</button>' +
          '<input id="b-path" value="' + esc(j.dir) + '" spellcheck="false" autocomplete="off" style="flex:1">' +
          '<button class="btn" id="b-go">Go</button>' +
        '</div>' +
        (j.error ? '<div class="hint">' + esc(j.error) + '</div>' : '') +
        '<div class="keylist">' +
          dirs.map((e: ApiTunnelsBrowseEntry): string => { return '<button class="is-dir" data-dir="' + esc(e.path) + '">📁 ' + esc(e.name) + '</button>'; }).join("") +
          files.map((e: ApiTunnelsBrowseEntry): string => { return '<button data-file="' + esc(e.path) + '">📄 ' + esc(e.name) + '</button>'; }).join("") +
        '</div>' +
      '</div>' +
      '<div class="sheet-foot"><button class="btn" data-close>Cancel</button></div>';
    picker.querySelector<HTMLButtonElement>("[data-close]")!.onclick = close;
    if (j.parent) picker.querySelector<HTMLButtonElement>("#b-up")!.onclick = (): void => { cur = j?.parent!; void render(); };
    const go = (): void => { cur = $<HTMLInputElement>("b-path").value.trim(); void render(); };
    picker.querySelector<HTMLButtonElement>("#b-go")!.onclick = go;
    picker.querySelector<HTMLInputElement>("#b-path")!.onkeydown = (ev: KeyboardEvent): void => { if (ev.key === "Enter") go(); };
    Array.prototype.forEach.call(picker.querySelectorAll("[data-dir]"), (b: HTMLElement): void => {
      b.onclick = (): void => { cur = b.getAttribute("data-dir")!; void render(); };
    });
    Array.prototype.forEach.call(picker.querySelectorAll("[data-file]"), (b: HTMLElement): void => {
      b.onclick = (): void => { target.value = b.getAttribute("data-file")!; close(); };
    });
  }
  void render();
}

async function saveConn(existing: ApiTunnelConnectionRow | null): Promise<void> {
  const authType = $<HTMLSelectElement>("c-auth").value;
  const body: Record<string, unknown> = {
    name: $<HTMLInputElement>("c-name").value.trim(),
    host: $<HTMLInputElement>("c-host").value.trim(),
    port: Number($<HTMLInputElement>("c-port").value) || 22,
    username: $<HTMLInputElement>("c-user").value.trim(),
    authType: authType,
  };
  if (authType === "key") {
    body.keyPath = $<HTMLInputElement>("c-keypath").value.trim() || (state.tun.keys && state.tun.keys.defaultPath) || "";
    body.passphrase = $<HTMLInputElement>("c-pass").value;
  } else {
    body.password = $<HTMLInputElement>("c-pass").value;
  }
  // docs/27 §4: the Advanced fields are optional — a key rides the payload only while the
  // sheet holds a value; empty means unset, and clearing a stored value sends nothing (the
  // server rebuilds the def from the request body, so an absent key and an empty string
  // both land as "no value"). An untouched proxyPassword input still carries the mask
  // sentinel the row brought in; echoing it back unchanged is what keeps the stored secret
  // (unmask_conn restores it server-side), exactly the MCP sheet's sentinel habit.
  const proxy = $<HTMLInputElement>("c-proxy").value.trim();
  if (proxy) body.proxy = proxy;
  const proxyUser = $<HTMLInputElement>("c-proxy-user").value.trim();
  if (proxyUser) body.proxyUsername = proxyUser;
  const proxyPass = $<HTMLInputElement>("c-proxy-pass").value;
  if (proxyPass) body.proxyPassword = proxyPass;
  const jump = $<HTMLSelectElement>("c-jump").value;
  if (jump) body.jump = jump;
  // Read the sheet's Group before closeSheet wipes it: a create lands in the picked group
  // (remembered as this scope's last-used), the same pre-join the MCP sheet does.
  const picked = $("g-sel") ? $<HTMLSelectElement>("g-sel").value : null;
  // api() rather than apiJson: a refused save (the §1.3 family — a bad proxy URL, a jump
  // cycle, proxy and jump together) must land INLINE beside the fields that caused it, not
  // only in a toast, and the sheet stays open so the fix is a keystroke away.
  let j: { error?: string; connection?: ApiTunnelConnectionRow };
  try {
    const r = await api(existing
      ? "/api/tunnels/connections/" + encodeURIComponent(existing.id)
      : "/api/tunnels/connections", { method: existing ? "PUT" : "POST", body: JSON.stringify(body) });
    j = await r.json().catch((): object => { return {}; }) as { error?: string; connection?: ApiTunnelConnectionRow };
    if (!r.ok) {
      const err = $("c-err");
      if (err) {
        err.hidden = false;
        err.textContent = j.error || "HTTP " + r.status;
        err.style.color = "var(--red)";
      }
      return;
    }
  } catch (e) {
    toast("request failed — is the gateway running?", true);
    return;
  }
  if (picked) rememberGroup("conns", picked);
  closeSheet();
  await loadTunnels();
  if (!existing && picked && j?.connection && j?.connection.id) await assignTunScoped("conns", j?.connection.id, picked);
  toast((existing ? "Saved " : "Added ") + body.name);
}

/* --- rule sheet ------------------------------------------------------------------------------- */

function openRuleSheet(def: ApiTunnelRuleRow | null): void {
  const d = tunData();
  if (!d.connections.length) { toast("Add an SSH connection first", true); return; }
  const editing = !!def;
  const r: RuleSheetDraft = def || {
    name: "", connectionId: d.connections[0].id, localPort: "", targetHost: "127.0.0.1", targetPort: "",
    remark: "", autoReconnect: false, reconnectInterval: 10, mcps: [],
  };
  // Same contract as the connection sheet: a Group select on create only, preselected from
  // the header + that opened this, else the scope's last-used group.
  const names = tunGroupsList();
  const initial = state.tun.pendingGroup || resolveDefaultGroup(names, lastGroup("rules"));
  state.tun.pendingGroup = null;
  const groupField = editing ? "" : groupFieldHtml(names, initial);
  const opts = d.connections.map((c: ApiTunnelConnectionRow): string => {
    return '<option value="' + esc(c.id) + '"' + (c.id === r.connectionId ? " selected" : "") + ">" + esc(c.name) + "</option>";
  }).join("");
  $("sheet").innerHTML =
    '<div class="sheet" role="dialog" aria-modal="true" aria-label="' + (editing ? "Edit rule" : "New rule") + '">' +
      '<div class="sheet-head"><h2 id="t-title">' + (editing ? "Edit forwarding rule" : "New forwarding rule in " + esc(initial)) + "</h2></div>" +
      '<div class="sheet-body">' +
        '<label class="field"><span>Name</span><input id="r-name" value="' + esc(r.name) + '" placeholder="Test server PostgreSQL" autocomplete="off"></label>' +
        groupField +
        '<label class="field"><span>SSH connection</span><select id="r-conn">' + opts + "</select></label>" +
        '<label class="field"><span>Local port</span><input id="r-lport" value="' + esc(String(r.localPort)) + '" placeholder="5433" autocomplete="off"></label>' +
        '<div class="two">' +
          '<label class="field"><span>Target host</span><input id="r-thost" value="' + esc(r.targetHost) + '" placeholder="127.0.0.1" autocomplete="off"></label>' +
          '<label class="field"><span>Target port</span><input id="r-tport" value="' + esc(String(r.targetPort)) + '" placeholder="5432" autocomplete="off"></label>' +
        "</div>" +
        '<label class="field"><span>Remark</span><input id="r-remark" value="' + esc(r.remark || "") + '" placeholder="Optional" autocomplete="off"></label>' +
        '<div class="cap" style="padding-top:var(--s2)">Serves MCPs</div>' +
        '<div class="mcp-picks" id="r-mcps"></div>' +
        '<div class="hint" id="r-mcps-hint">Which MCPs use this tunnel. Shown on both sides, and you are warned before stopping a tunnel an MCP is using — nothing is started or restarted for you.</div>' +
        '<div class="cap" style="padding-top:var(--s2)">Advanced</div>' +
        '<label class="check"><input type="checkbox" id="r-auto"' + (r.autoReconnect ? " checked" : "") + ">Reconnect automatically</label>" +
        '<label class="field"><span>Reconnect interval (seconds)</span><input id="r-interval" value="' + esc(String(r.reconnectInterval || 10)) + '" autocomplete="off"></label>' +
        '<div class="hint">A dropped tunnel releases its local port first, then retries. Authentication and host-key failures are never retried.</div>' +
      "</div>" +
      '<div class="sheet-foot"><button class="btn" id="r-cancel">Cancel</button>' +
        '<button class="btn primary" id="r-save">Save</button></div>' +
    "</div>";
  $("sheet").hidden = false;

  paintMcpPicks(r.mcps || [], []);
  // For a new rule, the suggestion is the point: type 5433 and the matching MCP checks itself.
  const suggest = async (): Promise<void> => {
    const port = Number($<HTMLInputElement>("r-lport").value);
    if (!port) return;
    const j = await apiJson<ApiTunnelsSuggestResponse>("/api/tunnels/suggest/" + port);
    if (!j) return;
    const checked = editing ? readMcpPicks() : j.mcps as string[];
    paintMcpPicks(checked, j.mcps as string[]);
  };
  if (!editing) $<HTMLInputElement>("r-lport").onchange = suggest;
  else void suggest(); // editing: keep the stored choice, but label what matches
  if ($("g-sel")) $<HTMLSelectElement>("g-sel").onchange = (): void => {
    $("t-title").textContent = "New forwarding rule in " + $<HTMLSelectElement>("g-sel").value;
  };
  $<HTMLButtonElement>("r-cancel").onclick = closeSheet;
  $<HTMLButtonElement>("r-save").onclick = (): void => { void saveRule(def!); };
  $("sheet").onclick = (e: MouseEvent): void => { if (e.target === $("sheet")) closeSheet(); };
  $<HTMLInputElement>("r-name").focus();
}

function paintMcpPicks(checked: string[], suggested: string[]): void {
  const names = tunData().mcps || [];
  if (!names.length) {
    $("r-mcps").innerHTML = '<div class="hint">No MCPs registered.</div>';
    return;
  }
  $("r-mcps").innerHTML = names.map((n: string): string => {
    const on = checked.indexOf(n) >= 0;
    const hint = suggested.indexOf(n) >= 0 ? ' <span class="hint">(matches this local port)</span>' : "";
    return '<label class="check"><input type="checkbox" data-mcp="' + esc(n) + '"' + (on ? " checked" : "") + ">" +
      esc(n) + hint + "</label>";
  }).join("");
}

function readMcpPicks(): string[] {
  const out: string[] = [];
  Array.prototype.forEach.call($("r-mcps").querySelectorAll("[data-mcp]"), (b: HTMLInputElement): void => {
    if (b.checked) out.push(b.dataset.mcp!);
  });
  return out;
}

async function saveRule(existing: ApiTunnelRuleRow): Promise<void> {
  const body = {
    name: $<HTMLInputElement>("r-name").value.trim(),
    connectionId: $<HTMLSelectElement>("r-conn").value,
    localPort: Number($<HTMLInputElement>("r-lport").value),
    targetHost: $<HTMLInputElement>("r-thost").value.trim() || "127.0.0.1",
    targetPort: Number($<HTMLInputElement>("r-tport").value),
    remark: $<HTMLInputElement>("r-remark").value.trim(),
    autoReconnect: $<HTMLInputElement>("r-auto").checked,
    reconnectInterval: Number($<HTMLInputElement>("r-interval").value) || 10,
    mcps: readMcpPicks(),
  };
  if (!body.targetPort) body.targetPort = body.localPort;
  const picked = $("g-sel") ? $<HTMLSelectElement>("g-sel").value : null;
  const j = existing
    ? await apiJson<ApiTunnelRuleMutation>("/api/tunnels/rules/" + encodeURIComponent(existing.id), { method: "PUT", body: JSON.stringify(body) })
    : await apiJson<ApiTunnelRuleMutation>("/api/tunnels/rules", { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  if (picked) rememberGroup("rules", picked);
  closeSheet();
  await loadTunnels();
  const row = j.rule || {} as ApiTunnelRuleRow;
  // A create lands in the picked group, not wherever the header + promised.
  if (!existing && picked && row.id) await assignTunScoped("rules", row.id, picked);
  toast((existing ? "Saved " : "Added ") + body.name + (row.state && row.state !== "stopped" ? " (" + row.state + ")" : ""));
}

export { loadKeys, openConnSheet, openKeyPicker, openRuleSheet, paintMcpPicks, readMcpPicks, saveConn, saveRule };
