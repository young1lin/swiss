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

import type { ApiMcpRow, ApiTokenCreated, ApiTokenSecret } from "./types/api.js";
import type { PhantomMcpRow } from "./types/dom.js";
import type { McpDetail } from "./types/state.js";
import { TOKEN_ID_KEY, apiJson, targetEl, toast } from "./util.js";
import { kindBodyNode, logsBodyNode } from "./logs.js";
import { frag } from "./h.js";
import { closeMenu } from "./pane.js";
import { configBody, histClose } from "./run-history.js";
import { runBody } from "./run.js";
import { activeTokenSecret, pickCopyToken, refreshTokens, rememberedTokenId, setActiveTokenSecret, tokenRows } from "./views/tokens.js";
import { menuIsOpen } from "./ui-state.js";
import { gatewayInfo, mcpDetail } from "./mcp-state.js";

/* --- connecting a client ---------------------------------------------------------------------- */
/**
 * Client configuration is built here, in the browser, from the endpoint URL and the live bearer
 * token (fetched once into views/tokens.ts' activeTokenSecret). Each copied command embeds the
 * token, so paste = ready — no separate step of exporting the token in whatever shell the command
 * lands in.
 */
function tokenEnv(): string {
  return gatewayInfo()?.tokenEnv || "SWISS_TOKEN";
}
function endpointUrl(name: string): string {
  // docs/24: /mcp/ is the MCP plugin's domain — every client URL this panel can produce
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
async function resolveSecret(): Promise<string | null> {
  await refreshTokens();
  const list = tokenRows();

  if (!list.length) {
    const made = await apiJson<ApiTokenCreated>("/api/tokens", { method: "POST", body: JSON.stringify({ label: "default" }) });
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

async function fetchSecret(id: string): Promise<string | null> {
  const j = await apiJson<ApiTokenSecret>("/api/tokens/" + encodeURIComponent(id) + "/secret");
  if (!j) return null;
  return useToken(j.id, j.secret);
}

/** Remember which token the copy actions embed — by id, so a rotate is picked up automatically. */
function useToken(id: string, secret: string): string {
  setActiveTokenSecret(secret);
  try { localStorage.setItem(TOKEN_ID_KEY, id); } catch (e) { /* blocked — this session still works */ }
  return secret;
}

/** Copy a connect command for one MCP, embedding the active token secret. */
async function copyConn(name: string, kind: string): Promise<void> {
  const secret = await resolveSecret();
  if (!secret) return;
  const text = kind === "claude" ? claudeSnippet(name, secret)
    : kind === "codex" ? codexSnippet(name, secret) : mcpJsonSnippet(name, secret);
  void copyText(text, kind === "claude" ? "Claude Code command" : kind === "codex" ? "Codex block" : ".mcp.json entry");
}

/** `claude mcp add` — one line, secret embedded. Runs in any shell (cmd, PowerShell, bash, zsh). */
function claudeSnippet(name: string, secret: string): string {
  return 'claude mcp add --transport http -s user ' + name + " " + endpointUrl(name) +
    ' --header "Authorization: Bearer ' + secret + '"';
}

/** Codex has no one-line add for an HTTP server, so copy this block into ~/.codex/config.toml. */
function codexSnippet(name: string, secret: string): string {
  return "[mcp_servers." + name + "]\n" +
    'type = "http"\n' +
    'url = "' + endpointUrl(name) + '"\n' +
    'headers = { "Authorization" = "Bearer ' + secret + '" }';
}

/** A `.mcp.json` entry for project-scoped Claude Code; the secret is inline. */
function mcpJsonSnippet(name: string, secret: string): string {
  const entry: Record<string, { type: string; url: string; headers: { Authorization: string } }> = {};
  entry[name] = { type: "http", url: endpointUrl(name), headers: { Authorization: "Bearer " + secret } };
  return JSON.stringify({ mcpServers: entry }, null, 2);
}

async function copyText(text: string, label: string): Promise<void> {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) await navigator.clipboard.writeText(text);
    else legacyCopy(text);
    toast(label + " copied — the token is embedded.");
  } catch (e) {
    legacyCopy(text);
    toast(label + " copied");
  }
}

/** Fallback for a browser that withholds the async clipboard (or a non-secure origin). */
function legacyCopy(text: string): void {
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
  if (menuIsOpen()) closeMenu();
  // The Run history popover closes on any click outside itself AND outside the popover — the
  // popover sits on <body>, so clicking its scrollbar or the preview pane must not count as "outside".
  // A row click closes it through applyRunHistory, its own handler.
  const inHist = targetEl(e)?.closest(".hist-wrap, .hist-pop");
  const d = mcpDetail();
  if (!inHist && d && d.tab === "run" && d.run.histOpen) histClose();
});

/** Staging bridge (docs/37 R5): the Logs/Tools/Resources/Prompts bodies are built as NODES
 *  now, while pane.ts still paints its column by string concatenation. Serialising a built
 *  tree is safe — text nodes escape on the way out — and the bridge disappears with pane's own
 *  conversion, the last piece of this view. */
function nodeHtml(node: Node): string {
  const host = document.createElement("div");
  host.append(node);
  return host.innerHTML;
}

function tabBody(d: McpDetail, m: ApiMcpRow | PhantomMcpRow): string {
  if (d.tab === "run") return runBody(d, m);
  if (d.tab === "config") return configBody(d);
  if (d.tab === "logs") return nodeHtml(frag(logsBodyNode(d)));
  return nodeHtml(frag(kindBodyNode(d, d.tab, m)));
}

export { claudeSnippet, codexSnippet, copyConn, copyText, endpointUrl, fetchSecret, legacyCopy, mcpJsonSnippet, resolveSecret, tabBody, tokenEnv, useToken };
