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

                                                                                                                                                                                                                                          
import { $, api, apiJson, el, toast } from "./util.js";
import { fill, h } from "./h.js";
                                     
import { closeSheet } from "./add-sheet.js";
import { loadTunnels, tunConnName, tunData, tunGroupsList } from "./polling.js";
import { assignTunScoped } from "./tunnels.js";
import { groupFieldNode, lastGroup, rememberGroup, resolveDefaultGroup } from "./groups.js";
import { setTunKeys, takeTunPendingGroup, tunKeys } from "./tunnel-state.js";
import { tr } from "./i18n.js";

/* --- connection sheet -------------------------------------------------------------------------- */

async function loadKeys()                                  {
  const cached = tunKeys();
  if (cached) return cached;
  const j = await apiJson                        ("/api/tunnels/keys");
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
function advancedConnNode(d                , editing         )           {
  const chips           = [];
  if (d.proxy) chips.push(" ", h("span", { class: "tag" }, tr("tunnelSheets.proxy")));
  if (d.jump) chips.push(" ", h("span", { class: "tag" }, tr("tunnelSheets.name", { name: tunConnName(d.jump) })));
  const jumpOpts           = [h("option", { value: "" }, tr("tunnelSheets.none"))].concat(tunData().connections
    .filter((c                        )          => { return !editing || c.id !== d.id; })
    .map((c                        ) => {
      return h("option", { value: c.id, selected: d.jump === c.id }, c.name);
    }));
  return [
    h("details", { class: "fold", id: "c-advanced" },
      h("summary", null, tr("tunnelSheets.advanced"), chips),
      h("div", { class: "fold-body" },
        h("div", { class: "cap" }, tr("tunnelSheets.proxy2")),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.proxyUrl")),
          h("input", { id: "c-proxy", value: d.proxy || "", placeholder: tr("tunnelSheets.socks5N127N0N0"), autocomplete: "off", spellcheck: false })),
        h("div", { class: "two" },
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.proxyUsername")),
            h("input", { id: "c-proxy-user", value: d.proxyUsername || "", autocomplete: "off" })),
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.proxyPassword")),
            h("input", { id: "c-proxy-pass", type: "password", value: d.proxyPassword || "", autocomplete: "off" }))),
        h("div", { class: "hint" }, tr("tunnelSheets.leaveEmptyConnectDirectly")),
        h("div", { class: "cap" }, tr("tunnelSheets.connectionJump")),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.connection")),
          h("select", { id: "c-jump" }, jumpOpts)),
        h("div", { class: "hint" }, tr("tunnelSheets.dialsThroughChosenConnection")))),
    h("div", { class: "hint", id: "c-err", hidden: true }),
  ];
}

function openConnSheet(def                               )       {
  const editing = !!def;
  const d                 = def || { name: "", host: "", port: 22, username: "", authType: "key", keyPath: "", passphrase: "", password: "" };
  // Creating: a Group select names where the row lands (the header +'s group preselected, the
  // last-used one when the top New opened this). Editing: no field - moving a connection is
  // the list's gesture (drag / row menu), not a property of its definition.
  const names = tunGroupsList();
  const initial = takeTunPendingGroup() || resolveDefaultGroup(names, lastGroup("conns"));
  const groupField = editing ? null : groupFieldNode(names, initial);
  // The house sheet idiom (panel-proof-of-life rule 1): visible BEFORE the body is painted.
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog",
        aria: { modal: "true", label: editing ? tr("tunnelSheets.editConnection") : tr("tunnelSheets.newConnection") } },
      h("div", { class: "sheet-head" },
        h("h2", { id: "t-title" }, editing ? tr("tunnelSheets.editConnection") : tr("tunnelSheets.newSshConnectionGroup", { group: initial }))),
      h("div", { class: "sheet-body" },
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.name2")),
          h("input", { id: "c-name", value: d.name, placeholder: tr("tunnelSheets.testServer"), autocomplete: "off" })),
        groupField,
        h("div", { class: "two" },
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.host")),
            h("input", { id: "c-host", value: d.host, placeholder: tr("tunnelSheets.eGN192N168"), autocomplete: "off" })),
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.port")),
            h("input", { id: "c-port", value: String(d.port || 22), autocomplete: "off" }))),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.username")),
          h("input", { id: "c-user", value: d.username, placeholder: tr("tunnelSheets.enterUsername"), autocomplete: "off" })),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.authentication")),
          h("select", { id: "c-auth" },
            h("option", { value: "key", selected: d.authType === "key" }, tr("tunnelSheets.keyPrivateKeyFile")),
            h("option", { value: "password", selected: d.authType === "password" }, tr("tunnelSheets.password")))),
        h("div", { id: "c-auth-fields" }),
        advancedConnNode(d, editing)),
      h("div", { class: "sheet-foot" },
        h("button", { class: "btn", id: "c-cancel" }, tr("tunnelSheets.cancel")),
        h("button", { class: "btn primary", id: "c-save" }, tr("tunnelSheets.save")))));

  const paint = async ()                => {
    const keys = await loadKeys();
    const isKey = $                   ("c-auth").value === "key";
    fill($("c-auth-fields"),
      isKey
        ? [h("div", { class: "with-btn" },
            h("label", { class: "field" },
              h("span", null, tr("tunnelSheets.privateKeyPath")),
              h("input", { id: "c-keypath", value: d.keyPath || "", placeholder: keys.defaultPath, autocomplete: "off", spellcheck: false })),
            h("button", { class: "btn", id: "c-browse" }, tr("tunnelSheets.browse"))),
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.passphraseOptional")),
            h("input", { id: "c-pass", type: "password", value: d.passphrase || "", placeholder: tr("tunnelSheets.leaveEmptyKeyNone"), autocomplete: "off" })),
          h("div", { class: "hint" }, tr("tunnelSheets.defaults"), h("code", null, keys.defaultPath), tr("tunnelSheets.whenLeftEmpty"))]
        : [h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.password2")),
            h("input", { id: "c-pass", type: "password", value: d.password || "", placeholder: tr("tunnelSheets.enterPassword"), autocomplete: "off" }))]);
    if ($("c-browse")) $                   ("c-browse").onclick = openKeyPicker;
  };
  void paint();
  $                   ("c-auth").onchange = ()       => { void paint(); };
  if ($("g-sel")) $                   ("g-sel").onchange = ()       => {
    $("t-title").textContent = tr("tunnelSheets.newSshConnectionGroup", { group: $                   ("g-sel").value });
  };
  $                   ("c-cancel").onclick = closeSheet;
  $                   ("c-save").onclick = ()       => { void saveConn(def ); };
  $("sheet").onclick = (e            )       => { if (e.target === $("sheet")) closeSheet(); };
  $                  ("c-name").focus();
}

/**
 * A server-side directory browser for the private key. A browser file picker can't return a real
 * server-side path (only a bare filename), so Browse is served from /api/tunnels/browse, which can
 * reach any folder the gateway process can read — not just ~/.ssh. Folders navigate; files pick.
 */
async function openKeyPicker()                {
  const target = $                  ("c-keypath");
  // Start in the current key's folder if one is set, otherwise let the backend default to ~/.ssh.
  let cur = target.value && target.value.trim() ? target.value.trim().replace(/[/\\][^/\\]*$/, "") : "";

  const back = el("div", "backdrop");
  back.style.zIndex = "60";
  const picker = el("div", "sheet");
  picker.setAttribute("role", "dialog");
  picker.setAttribute("aria-modal", "true");
  picker.setAttribute("aria-label", tr("tunnelSheets.choosePrivateKeyFile"));
  back.appendChild(picker);
  document.body.appendChild(back);
  const close = ()       => { document.body.removeChild(back); target.focus(); };
  back.onclick = (e            )       => { if (e.target === back) close(); };

  async function render()                {
    const j = await apiJson                          ("/api/tunnels/browse" + (cur ? "?dir=" + encodeURIComponent(cur) : ""));
    if (!j) { toast(tr("tunnelSheets.couldReadFolder"), true); return; }
    cur = j.dir; // normalize to the resolved path the server returned
    const dirs = j.entries.filter((e                       )          => { return e.dir; });
    const files = j.entries.filter((e                       )          => { return !e.dir; });
    fill(picker,
      h("div", { class: "sheet-head" }, h("h2", null, tr("tunnelSheets.choosePrivateKey"))),
      h("div", { class: "sheet-body" },
        h("div", { class: "browse-cwd", style: "display:flex;gap:8px;align-items:center;margin-bottom:8px" },
          // The disabled class is cosmetic; the disabled PROPERTY is what stops the click.
          h("button", { class: "btn" + (j.parent ? "" : " disabled"), id: "b-up", disabled: !j.parent, title: tr("tunnelSheets.oneLevel") }, tr("tunnelSheets.text")),
          h("input", { id: "b-path", value: j.dir, spellcheck: false, autocomplete: "off", style: "flex:1" }),
          h("button", { class: "btn", id: "b-go" }, tr("tunnelSheets.go"))),
        j.error ? h("div", { class: "hint" }, j.error) : null,
        h("div", { class: "keylist" },
          dirs.map((e                       )              => {
            return h("button", { class: "is-dir", data: { dir: e.path } }, "📁 ", e.name);
          }),
          files.map((e                       )              => {
            return h("button", { data: { file: e.path } }, "📄 ", e.name);
          }))),
      h("div", { class: "sheet-foot" }, h("button", { class: "btn", data: { close: "" } }, tr("tunnelSheets.cancel"))));
    picker.querySelector                   ("[data-close]") .onclick = close;
    if (j.parent) picker.querySelector                   ("#b-up") .onclick = ()       => { cur = j?.parent ; void render(); };
    const go = ()       => { cur = $                  ("b-path").value.trim(); void render(); };
    picker.querySelector                   ("#b-go") .onclick = go;
    picker.querySelector                  ("#b-path") .onkeydown = (ev               )       => { if (ev.key === "Enter") go(); };
    Array.prototype.forEach.call(picker.querySelectorAll("[data-dir]"), (b             )       => {
      b.onclick = ()       => { cur = b.getAttribute("data-dir") ; void render(); };
    });
    Array.prototype.forEach.call(picker.querySelectorAll("[data-file]"), (b             )       => {
      b.onclick = ()       => { target.value = b.getAttribute("data-file") ; close(); };
    });
  }
  void render();
}

async function saveConn(existing                               )                {
  const authType = $                   ("c-auth").value;
  const body                          = {
    name: $                  ("c-name").value.trim(),
    host: $                  ("c-host").value.trim(),
    port: Number($                  ("c-port").value) || 22,
    username: $                  ("c-user").value.trim(),
    authType: authType,
  };
  if (authType === "key") {
    body.keyPath = $                  ("c-keypath").value.trim() || tunKeys()?.defaultPath || "";
    body.passphrase = $                  ("c-pass").value;
  } else {
    body.password = $                  ("c-pass").value;
  }
  // docs/27 §4: the Advanced fields are optional — a key rides the payload only while the
  // sheet holds a value; empty means unset, and clearing a stored value sends nothing (the
  // server rebuilds the def from the request body, so an absent key and an empty string
  // both land as "no value"). An untouched proxyPassword input still carries the mask
  // sentinel the row brought in; echoing it back unchanged is what keeps the stored secret
  // (unmask_conn restores it server-side), exactly the MCP sheet's sentinel habit.
  const proxy = $                  ("c-proxy").value.trim();
  if (proxy) body.proxy = proxy;
  const proxyUser = $                  ("c-proxy-user").value.trim();
  if (proxyUser) body.proxyUsername = proxyUser;
  const proxyPass = $                  ("c-proxy-pass").value;
  if (proxyPass) body.proxyPassword = proxyPass;
  const jump = $                   ("c-jump").value;
  if (jump) body.jump = jump;
  // Read the sheet's Group before closeSheet wipes it: a create lands in the picked group
  // (remembered as this scope's last-used), the same pre-join the MCP sheet does.
  const picked = $("g-sel") ? $                   ("g-sel").value : null;
  // api() rather than apiJson: a refused save (the §1.3 family — a bad proxy URL, a jump
  // cycle, proxy and jump together) must land INLINE beside the fields that caused it, not
  // only in a toast, and the sheet stays open so the fix is a keystroke away.
  let j                                                         ;
  try {
    const r = await api(existing
      ? "/api/tunnels/connections/" + encodeURIComponent(existing.id)
      : "/api/tunnels/connections", { method: existing ? "PUT" : "POST", body: JSON.stringify(body) });
    j = await r.json().catch(()         => { return {}; })                                                           ;
    if (!r.ok) {
      const err = $("c-err");
      if (err) {
        err.hidden = false;
        err.textContent = j.error || tr("tunnelSheets.httpN", { n: r.status });
        err.style.color = "var(--red)";
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
  toast(existing ? tr("tunnelSheets.savedName", { name: body.name           }) : tr("tunnelSheets.addedName", { name: body.name           }));
}

/* --- rule sheet ------------------------------------------------------------------------------- */

function openRuleSheet(def                         )       {
  const d = tunData();
  if (!d.connections.length) { toast(tr("tunnelSheets.addSshConnectionFirst"), true); return; }
  const editing = !!def;
  const r                 = def || {
    name: "", connectionId: d.connections[0].id, localPort: "", targetHost: "127.0.0.1", targetPort: "",
    remark: "", autoReconnect: false, reconnectInterval: 10, mcps: [],
  };
  // Same contract as the connection sheet: a Group select on create only, preselected from
  // the header + that opened this, else the scope's last-used group.
  const names = tunGroupsList();
  const initial = takeTunPendingGroup() || resolveDefaultGroup(names, lastGroup("rules"));
  const groupField = editing ? null : groupFieldNode(names, initial);
  const connOpts = d.connections.map((c                        )              => {
    return h("option", { value: c.id, selected: c.id === r.connectionId }, c.name);
  });
  // The house sheet idiom: visible BEFORE the body is painted.
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog",
        aria: { modal: "true", label: editing ? tr("tunnelSheets.editRule") : tr("tunnelSheets.newRule") } },
      h("div", { class: "sheet-head" },
        h("h2", { id: "t-title" }, editing ? tr("tunnelSheets.editForwardingRule") : tr("tunnelSheets.newForwardingRuleGroup", { group: initial }))),
      h("div", { class: "sheet-body" },
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.name2")),
          h("input", { id: "r-name", value: r.name, placeholder: tr("tunnelSheets.testServerPostgresql"), autocomplete: "off" })),
        groupField,
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.sshConnection")),
          h("select", { id: "r-conn" }, connOpts)),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.localPort")),
          h("input", { id: "r-lport", value: String(r.localPort), placeholder: tr("tunnelSheets.n5433"), autocomplete: "off" })),
        h("div", { class: "two" },
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.targetHost")),
            h("input", { id: "r-thost", value: r.targetHost, placeholder: tr("tunnelSheets.n127N0N0N1"), autocomplete: "off" })),
          h("label", { class: "field" },
            h("span", null, tr("tunnelSheets.targetPort")),
            h("input", { id: "r-tport", value: String(r.targetPort), placeholder: tr("tunnelSheets.n5432"), autocomplete: "off" }))),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.remark")),
          h("input", { id: "r-remark", value: r.remark || "", placeholder: tr("tunnelSheets.optional"), autocomplete: "off" })),
        h("div", { class: "cap", style: "padding-top:var(--s2)" }, tr("tunnelSheets.servesMcps")),
        h("div", { class: "mcp-picks", id: "r-mcps" }),
        h("div", { class: "hint", id: "r-mcps-hint" }, tr("tunnelSheets.whichMcpsUseTunnel")),
        h("div", { class: "cap", style: "padding-top:var(--s2)" }, tr("tunnelSheets.advanced")),
        h("label", { class: "check" },
          h("input", { type: "checkbox", id: "r-auto", checked: r.autoReconnect }),
          tr("tunnelSheets.reconnectAutomatically")),
        h("label", { class: "field" },
          h("span", null, tr("tunnelSheets.reconnectIntervalSeconds")),
          h("input", { id: "r-interval", value: String(r.reconnectInterval || 10), autocomplete: "off" })),
        h("div", { class: "hint" }, tr("tunnelSheets.droppedTunnelReleasesLocal"))),
      h("div", { class: "sheet-foot" },
        h("button", { class: "btn", id: "r-cancel" }, tr("tunnelSheets.cancel")),
        h("button", { class: "btn primary", id: "r-save" }, tr("tunnelSheets.save")))));

  paintMcpPicks(r.mcps || [], []);
  // For a new rule, the suggestion is the point: type 5433 and the matching MCP checks itself.
  const suggest = async ()                => {
    const port = Number($                  ("r-lport").value);
    if (!port) return;
    const j = await apiJson                           ("/api/tunnels/suggest/" + port);
    if (!j) return;
    const checked = editing ? readMcpPicks() : j.mcps            ;
    paintMcpPicks(checked, j.mcps            );
  };
  if (!editing) $                  ("r-lport").onchange = suggest;
  else void suggest(); // editing: keep the stored choice, but label what matches
  if ($("g-sel")) $                   ("g-sel").onchange = ()       => {
    $("t-title").textContent = tr("tunnelSheets.newForwardingRuleGroup", { group: $                   ("g-sel").value });
  };
  $                   ("r-cancel").onclick = closeSheet;
  $                   ("r-save").onclick = ()       => { void saveRule(def ); };
  $("sheet").onclick = (e            )       => { if (e.target === $("sheet")) closeSheet(); };
  $                  ("r-name").focus();
}

function paintMcpPicks(checked          , suggested          )       {
  const names = tunData().mcps || [];
  if (!names.length) {
    fill($("r-mcps"), h("div", { class: "hint" }, tr("tunnelSheets.mcpsRegistered")));
    return;
  }
  fill($("r-mcps"), names.map((n        )              => {
    return h("label", { class: "check" },
      h("input", { type: "checkbox", data: { mcp: n }, checked: checked.includes(n) }),
      n,
      suggested.includes(n) ? h("span", { class: "hint" }, tr("tunnelSheets.matchesLocalPort")) : null);
  }));
}

function readMcpPicks()           {
  const out           = [];
  Array.prototype.forEach.call($("r-mcps").querySelectorAll("[data-mcp]"), (b                  )       => {
    if (b.checked) out.push(b.dataset.mcp );
  });
  return out;
}

async function saveRule(existing                  )                {
  const body = {
    name: $                  ("r-name").value.trim(),
    connectionId: $                   ("r-conn").value,
    localPort: Number($                  ("r-lport").value),
    targetHost: $                  ("r-thost").value.trim() || "127.0.0.1",
    targetPort: Number($                  ("r-tport").value),
    remark: $                  ("r-remark").value.trim(),
    autoReconnect: $                  ("r-auto").checked,
    reconnectInterval: Number($                  ("r-interval").value) || 10,
    mcps: readMcpPicks(),
  };
  if (!body.targetPort) body.targetPort = body.localPort;
  const picked = $("g-sel") ? $                   ("g-sel").value : null;
  const j = existing
    ? await apiJson                       ("/api/tunnels/rules/" + encodeURIComponent(existing.id), { method: "PUT", body: JSON.stringify(body) })
    : await apiJson                       ("/api/tunnels/rules", { method: "POST", body: JSON.stringify(body) });
  if (!j) return;
  if (picked) rememberGroup("rules", picked);
  closeSheet();
  await loadTunnels();
  const row = j.rule || {}                    ;
  // A create lands in the picked group, not wherever the header + promised.
  if (!existing && picked && row.id) await assignTunScoped("rules", row.id, picked);
  // Hoisted so the comparison literal stays out of the toast argument (i18n gate).
  const stateNote = row.state && row.state !== "stopped" ? tr("tunnelSheets.state", { state: row.state }) : "";
  toast((existing ? tr("tunnelSheets.savedName", { name: body.name }) : tr("tunnelSheets.addedName", { name: body.name })) + stateNote);
}

export { loadKeys, openConnSheet, openKeyPicker, openRuleSheet, paintMcpPicks, readMcpPicks, saveConn, saveRule };