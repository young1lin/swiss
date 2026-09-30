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

                                                                                 
                                                    
                                                  
import { TOKEN_ID_KEY, apiJson, isMcpKind, targetEl, toast } from "./util.js";
import { tr } from "./i18n.js";
import { kindBodyNode, logsBodyNode } from "./logs.js";
                                     
import { configBodyNode, histClose } from "./run-history.js";
import { runBodyNode } from "./run.js";
import { activeTokenSecret, pickCopyToken, refreshTokens, rememberedTokenId, setActiveTokenSecret, tokenRows } from "./views/tokens.js";
import { gatewayInfo, mcpDetail } from "./mcp-state.js";
import { closeMenu, menuOpen } from "./ui/menu.js";

/* --- connecting a client ---------------------------------------------------------------------- */
/**
 * Client configuration is built here, in the browser, from the endpoint URL and the live bearer
 * token (fetched once into views/tokens.ts' activeTokenSecret). Each copied command embeds the
 * token, so paste = ready — no separate step of exporting the token in whatever shell the command
 * lands in.
 */
function tokenEnv()         {
  return gatewayInfo()?.tokenEnv || "SWISS_TOKEN";
}
function endpointUrl(name        )         {
  // SPEC §mcp.endpoint: /mcp/ is the MCP plugin's domain — every client URL this panel can produce
  // goes through this one builder, so the prefix lives here and nowhere else.
  return location.origin + "/mcp/" + name;
}
/**
 * The secret a copied connect command embeds.
 *
 * Copies use the token last marked with Use, or — with no prior choice — the seed token labeled
 * `default`. That way a fresh panel with several tokens still copies without a prompt (the Cursor
 * IDE browser cannot answer `prompt()`, and a cancelled prompt used to look like a copy failure).
 * When there are no tokens at all it creates `default` rather than sending you to another panel.
 */
async function resolveSecret()                         {
  await refreshTokens();
  const list = tokenRows();

  if (!list.length) {
    const made = await apiJson                 ("/api/tokens", { method: "POST", body: JSON.stringify({ label: "default" }) });
    if (!made) return null;
    await refreshTokens();
    return useToken(made.id, made.secret);
  }

  const pick = pickCopyToken(list, rememberedTokenId());
  if (!pick) return null;
  const rememberedSecret = activeTokenSecret();
  if (rememberedSecret && rememberedTokenId() === pick.id) return rememberedSecret;
  return fetchSecret(pick.id);
}

async function fetchSecret(id        )                         {
  const j = await apiJson                ("/api/tokens/" + encodeURIComponent(id) + "/secret");
  if (!j) return null;
  return useToken(j.id, j.secret);
}

/** Remember which token the copy actions embed — by id, so a rotate is picked up automatically. */
function useToken(id        , secret        )         {
  setActiveTokenSecret(secret);
  try { localStorage.setItem(TOKEN_ID_KEY, id); } catch (e) { /* blocked — this session still works */ }
  return secret;
}

/** Copy a connect command for one MCP, embedding the active token secret. */
async function copyConn(name        , kind        )                {
  const secret = await resolveSecret();
  if (!secret) return;
  const text = kind === "claude" ? claudeSnippet(name, secret)
    : kind === "codex" ? codexSnippet(name, secret) : mcpJsonSnippet(name, secret);
  void copyText(text, kind === "claude" ? tr("connect.claudeCodeCommand") : kind === "codex" ? tr("connect.codexBlock") : tr("connect.mcpJsonEntry"), { token: true });
}

/** `claude mcp add` — one line, secret embedded. Runs in any shell (cmd, PowerShell, bash, zsh). */
function claudeSnippet(name        , secret        )         {
  return 'claude mcp add --transport http -s user ' + name + " " + endpointUrl(name) +
    ' --header "Authorization: Bearer ' + secret + '"';
}

/** Codex has no one-line add for an HTTP server, so copy this block into ~/.codex/config.toml. */
function codexSnippet(name        , secret        )         {
  return "[mcp_servers." + name + "]\n" +
    'type = "http"\n' +
    'url = "' + endpointUrl(name) + '"\n' +
    'headers = { "Authorization" = "Bearer ' + secret + '" }';
}

/** A `.mcp.json` entry for project-scoped Claude Code; the secret is inline. */
function mcpJsonSnippet(name        , secret        )         {
  const entry                                                                                    = {};
  entry[name] = { type: "http", url: endpointUrl(name), headers: { Authorization: "Bearer " + secret } };
  return JSON.stringify({ mcpServers: entry }, null, 2);
}

/** Copy one value and say so. `token` is for a text that carries the token secret inside it (a
 *  connect snippet): the toast says it is embedded, so nobody pastes it somewhere public by
 *  accident. Everything else - a secret reference, an endpoint URL, a port, the bare token -
 *  is just "copied": the success path used to claim an embedded token for every copy in the
 *  panel (found on a walk of SPEC §panel.settings, on Secrets' Copy ref). */
async function copyText(text        , label        , o                      = {})                {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) await navigator.clipboard.writeText(text);
    else legacyCopy(text);
  } catch (e) {
    legacyCopy(text);
  }
  toast(tr(o.token ? "connect.labelCopiedTokenEmbedded" : "connect.labelCopied", { label }));
}

/** Fallback for a browser that withholds the async clipboard (or a non-secure origin). */
function legacyCopy(text        )       {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); } catch (e) { /* nothing else to try */ }
  document.body.removeChild(ta);
}
document.addEventListener("click", (e) => {
  if (menuOpen()) closeMenu();
  // The Run history popover closes on any click outside itself AND outside the popover — the
  // popover sits on <body>, so clicking its scrollbar or the preview pane must not count as "outside".
  // A row click closes it through applyRunHistory, its own handler.
  const inHist = targetEl(e)?.closest(".hist-wrap, .hist-pop");
  const d = mcpDetail();
  if (!inHist && d && d.tab === "run" && d.run.histOpen) histClose();
});

/** The tab body as built nodes (SPEC §panel.toolchain). The staging bridge that used to serialise
 *  these back into pane's string paint retired with pane's own conversion: #tabbody is
 *  filled with the tree itself, nothing is parsed on the way in. */
function tabBody(d           , m                           )         {
  if (d.tab === "run") return runBodyNode(d, m);
  if (d.tab === "config") return configBodyNode(d);
  if (d.tab === "logs") return logsBodyNode(d);
  // The three kind pages are what is left; an unknown tab string paints nothing rather than
  // indexing a slot that does not exist.
  return isMcpKind(d.tab) ? kindBodyNode(d, d.tab, m) : null;
}

export { claudeSnippet, codexSnippet, copyConn, copyText, endpointUrl, fetchSecret, legacyCopy, mcpJsonSnippet, resolveSecret, tabBody, tokenEnv, useToken };