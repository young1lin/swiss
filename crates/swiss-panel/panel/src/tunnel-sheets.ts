/*
 * Copyright 2026 young1lin
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
import { $, api, apiJson, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import { loadTunnels, tunConnName, tunData, tunGroupsList } from "./polling.js";
import { assignTunScoped } from "./tunnels.js";
import { groupFieldNode, lastGroup, rememberGroup, resolveDefaultGroup } from "./groups.js";
import { setTunKeys, takeTunPendingGroup, tunKeys } from "./tunnel-state.js";
import { tr } from "./i18n.js";
import { btn, checkField, closeSheet, field, formCap, formFold, hint, inlineForm, pair, sheet, showSheet, stackSheet, tag } from "./ui/index.js";

/* --- connection sheet -------------------------------------------------------------------------- */

async function loadKeys(): Promise<ApiTunnelsKeysResponse> {
  const cached = tunKeys();
  if (cached) return cached;
  const j = await apiJson<ApiTunnelsKeysResponse>("/api/tunnels/keys");
  const keys = j || { keys: [], defaultPath: "" };
  setTunKeys(keys);
  return keys;
}

/** docs/27 §4: the Advanced fold — proxy and jump live here, collapsed by default. The
 *  summary chips keep "goes through a proxy / via <jump>" visible without unfolding, so
 *  collapsed never means hidden; with nothing configured the fold is the sheet's only new
 *  line. Native details/summary is the house fold primitive (the data-value JSON tree) —
 *  no open property, so the browser starts it folded. The jump dropdown lists every other
 *  connection (this one excluded); the backend stays the single source of truth for cycles,
 *  its 400 lands inline in the sheet, and the panel does not pre-walk chains.
 *
 *  Built, not concatenated (docs/37 R5); returns TWO siblings (the fold and the inline
 *  error line), which h() flattens into the sheet body. */
function advancedConnNode(d: ConnSheetDraft, editing: boolean): HChild[] {
  const chips: HChild[] = [];
  if (d.proxy) chips.push(" ", tag(tr("tunnelSheets.proxy")));
  if (d.jump) chips.push(" ", tag(tr("tunnelSheets.name", { name: tunConnName(d.jump) })));
  const jumpOpts: HChild[] = [h("option", { value: "" }, tr("tunnelSheets.none"))].concat(tunData().connections
    .filter((c: ApiTunnelConnectionRow): boolean => { return !editing || c.id !== d.id; })
    .map((c: ApiTunnelConnectionRow) => {
      return h("option", { value: c.id, selected: d.jump === c.id }, c.name);
    }));
  return [
    formFold({ summary: [tr("tunnelSheets.advanced"), chips], id: "c-advanced" },
        formCap(tr("tunnelSheets.proxy2")),
        field({
          label: tr("tunnelSheets.proxyUrl"),
          control: h("input", { id: "c-proxy", value: d.proxy || "", placeholder: tr("tunnelSheets.socks5N127N0N0"), autocomplete: "off", spellcheck: false }),
        }),
        pair(
          field({ label: tr("tunnelSheets.proxyUsername"), control: h("input", { id: "c-proxy-user", value: d.proxyUsername || "", autocomplete: "off" }) }),
          field({ label: tr("tunnelSheets.proxyPassword"), control: h("input", { id: "c-proxy-pass", type: "password", value: d.proxyPassword || "", autocomplete: "off" }) })),
        hint(tr("tunnelSheets.leaveEmptyConnectDirectly")),
        formCap(tr("tunnelSheets.connectionJump")),
        field({
          label: tr("tunnelSheets.connection"), control: h("select", { id: "c-jump" }, jumpOpts),
          hint: tr("tunnelSheets.dialsThroughChosenConnection"),
        })),
    // A refused save lands here, beside the fields that caused it (saveConn).
    hint(null, { id: "c-err", bad: true, hidden: true, live: true }),
  ];
}

function openConnSheet(def: ApiTunnelConnectionRow | null): void {
  const editing = !!def;
  const d: ConnSheetDraft = def || { name: "", host: "", port: 22, username: "", authType: "key", keyPath: "", passphrase: "", password: "" };
  // Creating: a Group select names where the row lands (the header +'s group preselected, the
  // last-used one when the top New opened this). Editing: no field - moving a connection is
  // the list's gesture (drag / row menu), not a property of its definition.
  const names = tunGroupsList();
  const initial = takeTunPendingGroup() || resolveDefaultGroup(names, lastGroup("conns"));
  const groupField = editing ? null : groupFieldNode(names, initial);
  // The library sheet (showSheet: visible BEFORE painted, the backdrop closes).
  showSheet(sheet({
    title: editing ? tr("tunnelSheets.editConnection") : tr("tunnelSheets.newSshConnectionGroup", { group: initial }),
    titleId: "t-title",
    label: editing ? tr("tunnelSheets.editConnection") : tr("tunnelSheets.newConnection"),
    body: [
      field({ label: tr("tunnelSheets.name2"), control: h("input", { id: "c-name", value: d.name, placeholder: tr("tunnelSheets.testServer"), autocomplete: "off" }) }),
      groupField,
      pair(
        field({ label: tr("tunnelSheets.host"), control: h("input", { id: "c-host", value: d.host, placeholder: tr("tunnelSheets.eGN192N168"), autocomplete: "off" }) }),
        field({ label: tr("tunnelSheets.port"), control: h("input", { id: "c-port", value: String(d.port || 22), autocomplete: "off" }) })),
      field({ label: tr("tunnelSheets.username"), control: h("input", { id: "c-user", value: d.username, placeholder: tr("tunnelSheets.enterUsername"), autocomplete: "off" }) }),
      field({
        label: tr("tunnelSheets.authentication"),
        control: h("select", { id: "c-auth" },
          h("option", { value: "key", selected: d.authType === "key" }, tr("tunnelSheets.keyPrivateKeyFile")),
          h("option", { value: "password", selected: d.authType === "password" }, tr("tunnelSheets.password"))),
      }),
      h("div", { id: "c-auth-fields" }),
      advancedConnNode(d, editing),
    ],
    foot: [btn(tr("tunnelSheets.cancel"), { id: "c-cancel" }), btn(tr("tunnelSheets.save"), { kind: "primary", id: "c-save" })],
  }));

  const paint = async (): Promise<void> => {
    const keys = await loadKeys();
    const isKey = $<HTMLSelectElement>("c-auth").value === "key";
    fill($("c-auth-fields"),
      isKey
        ? [field({
            label: tr("tunnelSheets.privateKeyPath"),
            control: h("input", { id: "c-keypath", value: d.keyPath || "", placeholder: keys.defaultPath, autocomplete: "off", spellcheck: false }),
            action: btn(tr("tunnelSheets.browse"), { id: "c-browse" }),
          }),
          field({
            label: tr("tunnelSheets.passphraseOptional"),
            control: h("input", { id: "c-pass", type: "password", value: d.passphrase || "", placeholder: tr("tunnelSheets.leaveEmptyKeyNone"), autocomplete: "off" }),
            hint: [tr("tunnelSheets.defaults"), h("code", null, keys.defaultPath), tr("tunnelSheets.whenLeftEmpty")],
          })]
        : [field({
            label: tr("tunnelSheets.password2"),
            control: h("input", { id: "c-pass", type: "password", value: d.password || "", placeholder: tr("tunnelSheets.enterPassword"), autocomplete: "off" }),
          })]);
    if ($("c-browse")) $<HTMLButtonElement>("c-browse").onclick = openKeyPicker;
  };
  void paint();
  $<HTMLSelectElement>("c-auth").onchange = (): void => { void paint(); };
  if ($("g-sel")) $<HTMLSelectElement>("g-sel").onchange = (): void => {
    $("t-title").textContent = tr("tunnelSheets.newSshConnectionGroup", { group: $<HTMLSelectElement>("g-sel").value });
  };
  $<HTMLButtonElement>("c-cancel").onclick = closeSheet;
  $<HTMLButtonElement>("c-save").onclick = (): void => { void saveConn(def!); };
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

  // A second layer over the connection sheet (ui/sheet.ts stackSheet): its own backdrop, and
  // Escape closes the picker alone. Closing hands focus back to the path field it fills.
  const layer = stackSheet((): void => { target.focus(); });
  const close = layer.close;

  async function render(): Promise<void> {
    const j = await apiJson<ApiTunnelsBrowseResponse>("/api/tunnels/browse" + (cur ? "?dir=" + encodeURIComponent(cur) : ""));
    if (!j) { toast(tr("tunnelSheets.couldReadFolder"), true); return; }
    cur = j.dir; // normalize to the resolved path the server returned
    const dirs = j.entries.filter((e: ApiTunnelsBrowseEntry): boolean => { return e.dir; });
    const files = j.entries.filter((e: ApiTunnelsBrowseEntry): boolean => { return !e.dir; });
    const picker = layer.paint({
      title: tr("tunnelSheets.choosePrivateKey"),
      label: tr("tunnelSheets.choosePrivateKeyFile"),
      body: [
        // One row: up a level, the path (it takes the rest), Go - an inline form in all but name.
        inlineForm(
          // The disabled PROPERTY is what stops the click.
          btn(tr("tunnelSheets.text"), { icon: "arrow-up", id: "b-up", disabled: !j.parent, title: tr("tunnelSheets.oneLevel") }),
          h("input", { id: "b-path", value: j.dir, spellcheck: false, autocomplete: "off" }),
          btn(tr("tunnelSheets.go"), { id: "b-go" })),
        j.error ? hint(j.error, { bad: true }) : null,
        h("div", { class: "keylist" },
          dirs.map((e: ApiTunnelsBrowseEntry): HTMLElement => {
            return h("button", { class: "is-dir", data: { dir: e.path } }, iconNode("folder"), " ", e.name);
          }),
          files.map((e: ApiTunnelsBrowseEntry): HTMLElement => {
            return h("button", { data: { file: e.path } }, iconNode("file"), " ", e.name);
          })),
      ],
      foot: btn(tr("tunnelSheets.cancel"), { data: { close: "" } }),
    });
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
    body.keyPath = $<HTMLInputElement>("c-keypath").value.trim() || tunKeys()?.defaultPath || "";
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
        err.textContent = j.error || tr("tunnelSheets.httpN", { n: r.status });
      }
      return;
    }
  } catch (e) {
    toast(tr("tunnelSheets.requestFailedGatewayRunning"), true);
    return;
  }
  if (picked) rememberGroup("conns", picked);
  closeSheet();
  await loadTunnels();
  if (!existing && picked && j?.connection && j?.connection.id) await assignTunScoped("conns", j?.connection.id, picked);
  toast(existing ? tr("tunnelSheets.savedName", { name: body.name as string }) : tr("tunnelSheets.addedName", { name: body.name as string }));
}

/* --- rule sheet ------------------------------------------------------------------------------- */

function openRuleSheet(def: ApiTunnelRuleRow | null): void {
  const d = tunData();
  if (!d.connections.length) { toast(tr("tunnelSheets.addSshConnectionFirst"), true); return; }
  const editing = !!def;
  const r: RuleSheetDraft = def || {
    name: "", connectionId: d.connections[0].id, localPort: "", targetHost: "127.0.0.1", targetPort: "",
    remark: "", autoReconnect: false, reconnectInterval: 10, mcps: [],
  };
  // Same contract as the connection sheet: a Group select on create only, preselected from
  // the header + that opened this, else the scope's last-used group.
  const names = tunGroupsList();
  const initial = takeTunPendingGroup() || resolveDefaultGroup(names, lastGroup("rules"));
  const groupField = editing ? null : groupFieldNode(names, initial);
  const connOpts = d.connections.map((c: ApiTunnelConnectionRow): HTMLElement => {
    return h("option", { value: c.id, selected: c.id === r.connectionId }, c.name);
  });
  // The library sheet (showSheet: visible BEFORE painted, the backdrop closes).
  showSheet(sheet({
    title: editing ? tr("tunnelSheets.editForwardingRule") : tr("tunnelSheets.newForwardingRuleGroup", { group: initial }),
    titleId: "t-title",
    label: editing ? tr("tunnelSheets.editRule") : tr("tunnelSheets.newRule"),
    body: [
      field({ label: tr("tunnelSheets.name2"), control: h("input", { id: "r-name", value: r.name, placeholder: tr("tunnelSheets.testServerPostgresql"), autocomplete: "off" }) }),
      groupField,
      field({ label: tr("tunnelSheets.sshConnection"), control: h("select", { id: "r-conn" }, connOpts) }),
      field({ label: tr("tunnelSheets.localPort"), control: h("input", { id: "r-lport", value: String(r.localPort), placeholder: tr("tunnelSheets.n5433"), autocomplete: "off" }) }),
      pair(
        field({ label: tr("tunnelSheets.targetHost"), control: h("input", { id: "r-thost", value: r.targetHost, placeholder: tr("tunnelSheets.n127N0N0N1"), autocomplete: "off" }) }),
        field({ label: tr("tunnelSheets.targetPort"), control: h("input", { id: "r-tport", value: String(r.targetPort), placeholder: tr("tunnelSheets.n5432"), autocomplete: "off" }) })),
      field({ label: tr("tunnelSheets.remark"), control: h("input", { id: "r-remark", value: r.remark || "", placeholder: tr("tunnelSheets.optional"), autocomplete: "off" }) }),
      formCap(tr("tunnelSheets.servesMcps")),
      h("div", { class: "mcp-picks", id: "r-mcps" }),
      hint(tr("tunnelSheets.whichMcpsUseTunnel"), { id: "r-mcps-hint" }),
      formCap(tr("tunnelSheets.advanced")),
      checkField({ label: tr("tunnelSheets.reconnectAutomatically"), control: h("input", { type: "checkbox", id: "r-auto", checked: r.autoReconnect }) as HTMLInputElement }),
      field({
        label: tr("tunnelSheets.reconnectIntervalSeconds"),
        control: h("input", { id: "r-interval", value: String(r.reconnectInterval || 10), autocomplete: "off" }),
        hint: tr("tunnelSheets.droppedTunnelReleasesLocal"),
      }),
    ],
    foot: [btn(tr("tunnelSheets.cancel"), { id: "r-cancel" }), btn(tr("tunnelSheets.save"), { kind: "primary", id: "r-save" })],
  }));

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
    $("t-title").textContent = tr("tunnelSheets.newForwardingRuleGroup", { group: $<HTMLSelectElement>("g-sel").value });
  };
  $<HTMLButtonElement>("r-cancel").onclick = closeSheet;
  $<HTMLButtonElement>("r-save").onclick = (): void => { void saveRule(def!); };
  $<HTMLInputElement>("r-name").focus();
}

function paintMcpPicks(checked: string[], suggested: string[]): void {
  const names = tunData().mcps || [];
  if (!names.length) {
    fill($("r-mcps"), hint(tr("tunnelSheets.mcpsRegistered")));
    return;
  }
  // One check per MCP; the one whose port matches the rule's wears a quiet tag, not a colour.
  fill($("r-mcps"), names.map((n: string): HTMLElement => {
    return checkField({
      label: suggested.includes(n) ? [n, " ", tag(tr("tunnelSheets.matchesLocalPort"))] : n,
      control: h("input", { type: "checkbox", data: { mcp: n }, checked: checked.includes(n) }) as HTMLInputElement,
    });
  }));
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
  // Hoisted so the comparison literal stays out of the toast argument (i18n gate).
  const stateNote = row.state && row.state !== "stopped" ? tr("tunnelSheets.state", { state: row.state }) : "";
  toast((existing ? tr("tunnelSheets.savedName", { name: body.name }) : tr("tunnelSheets.addedName", { name: body.name })) + stateNote);
}

export { loadKeys, openConnSheet, openKeyPicker, openRuleSheet, paintMcpPicks, readMcpPicks, saveConn, saveRule };