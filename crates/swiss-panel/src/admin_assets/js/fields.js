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
import { tk, tr } from "./i18n.js";
import { h } from "./h.js";
                                     

/* --- field schemas ---------------------------------------------------------------------------- */
/* `description` leads every type: it is what the MCP client is told this endpoint is for, and with
   several instances of the same engine behind identical tool sets, it is the only thing that says
   which application's data is on the other end. */
const DESC_FIELD            = {
  k: "description", label: tk("Description"),
  ph: tk("What this MCP is for — e.g. the order service's Redis, used by the checkout backend"),
  hint: tk("Describes the MCP itself. Sent to clients as the server's instructions, together with the connection target."),
};
/* "Start automatically at boot" — the user-facing side of the registry's `lazy` (inverted). A proc
   defaults OFF (its child is the one expensive idle thing here, so it starts on first request and
   is reaped when idle); every other type defaults ON. readFields returns it as `autostart`;
   submitAdd/saveEdit translate it to `lazy` before the server sees it. */
const AUTOSTART_PROC            = {
  k: "autostart", label: tk("Start automatically at boot"), bool: true, def: false,
  hint: tk("Off by default: the first client request spawns the child, and an idle one is reaped after 10 min (idleMs tunes). Adding it here still starts it now."),
};
const AUTOSTART_EAGER            = {
  k: "autostart", label: tk("Start automatically at boot"), bool: true, def: true,
  hint: tk("Off: idle at boot — the first client request starts it."),
};
const TESTABLE_TYPES           = ["mysql", "mariadb", "redis", "pg", "http", "rest"];
const TYPE_FIELDS                              = {
  proc: [
    DESC_FIELD,
    { k: "command", label: tk("Command"), ph: "npx -y @modelcontextprotocol/server-git   ·   uvx mcp-server-git" },
    { k: "cwd", label: tk("Working directory"), ph: "optional" },
    { k: "env", label: tk("Environment (KEY=VALUE per line)"), area: true, kv: true, ph: "GIT_REPO=D:\\dev\\project" },
    { k: "exposeResources", label: tk("Expose resources"), bool: true, def: true, hint: tk("Turn off for a child that publishes thousands of resources.") },
    { k: "exposePrompts", label: tk("Expose prompts"), bool: true, def: true },
    { k: "timeoutMs", label: tk("Call timeout (ms)"), num: true, ph: "180000", hint: tk("How long one tool call may run. Raise it for slow work — image analysis and long scrapes routinely pass a minute.") },
    AUTOSTART_PROC,
  ],
  mysql: [
    DESC_FIELD,
    { k: "host", label: tk("Host"), half: true }, { k: "port", label: tk("Port"), num: true, half: true },
    { k: "user", label: tk("User"), half: true }, { k: "password", label: tk("Password"), half: true },
    { k: "database", label: tk("Database"), half: true }, { k: "timezone", label: tk("Timezone"), half: true, ph: "Z" },
    { k: "maxRows", label: tk("Default row limit"), num: true, half: true, ph: "200" },
    AUTOSTART_EAGER,
  ],
  // docs/29: MariaDB speaks the MySQL wire protocol — this form is the mysql one verbatim
  // (the server maps the type to the same engine); only the type string and the seal differ.
  mariadb: [
    DESC_FIELD,
    { k: "host", label: tk("Host"), half: true }, { k: "port", label: tk("Port"), num: true, half: true },
    { k: "user", label: tk("User"), half: true }, { k: "password", label: tk("Password"), half: true },
    { k: "database", label: tk("Database"), half: true }, { k: "timezone", label: tk("Timezone"), half: true, ph: "Z" },
    { k: "maxRows", label: tk("Default row limit"), num: true, half: true, ph: "200" },
    AUTOSTART_EAGER,
  ],
  redis: [
    DESC_FIELD,
    { k: "host", label: tk("Host"), half: true }, { k: "port", label: tk("Port"), num: true, half: true },
    { k: "password", label: tk("Password"), half: true }, { k: "db", label: tk("DB index"), num: true, half: true },
    { k: "allowDestructive", label: tk("Allow FLUSHALL / FLUSHDB"), bool: true },
    { k: "allowEval", label: tk("Allow Lua (EVAL / FCALL)"), bool: true, hint: tk("A script is opaque to every other rule here — it can reach anything they refuse.") },
    AUTOSTART_EAGER,
  ],
  // docs/30: the def keeps ONE url string; the form edits the pieces. parse/serialize below,
  // the same pair on open and on save, so an edit touches exactly the field it meant to.
  pg: [
    DESC_FIELD,
    { k: "host", label: tk("Host"), half: true }, { k: "port", label: tk("Port"), num: true, half: true },
    { k: "user", label: tk("User"), half: true },
    {
      k: "password", label: tk("Password"), half: true,
      hint: tk("A literal, or a ${ENV_VAR} / ${secret://name} ref — the ref is stored, the value never is."),
    },
    { k: "database", label: tk("Database"), half: true }, { k: "maxRows", label: tk("Default row limit"), num: true, half: true, ph: "200" },
    { k: "params", label: tk("Options (k=v per line)"), area: true, ph: "sslmode=disable" },
    AUTOSTART_EAGER,
  ],
  http: [
    DESC_FIELD,
    { k: "url", label: tk("Endpoint URL"), area: true, ph: "https://mcp.context7.com/mcp" },
    {
      k: "headers", label: tk("Headers (NAME=VALUE per line)"), area: true, kv: true,
      ph: "Authorization=Bearer ${CONTEXT7_API_KEY}",
      hint: tk("Where the remote's API key goes. Prefer a reference — ${secret://name} (store it once on the Plugins page) or ${ENV_VAR} — so neither this panel nor managed.json ever holds the value."),
    },
    /* OAuth (docs/24): the checkbox is the def's auth string — checked sends "oauth", and the
       gateway then owns Authorization end to end (register, consent, refresh). The headers
       field above must not carry Authorization when this is on; the server refuses the pair. */
    {
      k: "auth", label: tk("OAuth authorization (the gateway owns the token)"), bool: true, def: false,
      hint: tk("For remote MCPs behind OAuth (Figma: https://mcp.figma.com/mcp). Save first, then Authorize once from the detail view — the gateway registers a client, opens the consent page and refreshes tokens itself."),
    },
    {
      k: "oauthClientName", label: tk("OAuth client name"), ph: tk("empty = Claude Code"),
      hint: tk("Figma's registration accepts only \"Claude Code\" or \"Codex\". Leave empty for the default."),
    },
    {
      k: "proxy", label: tk("Proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("Route THIS MCP's requests through an HTTP(S) proxy — for an endpoint this machine cannot reach directly. Other MCPs are not affected; a ${ENV_VAR} reference works here too."),
    },
    { k: "exposeResources", label: tk("Expose resources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("Expose prompts"), bool: true, def: true },
    AUTOSTART_EAGER,
  ],
  /* The figma type (docs/24 rev): the form is a description and nothing else. The endpoint
     and the OAuth mode are the type's to decide — the server refuses a url or auth key on a
     figma def — and the detail view's Authorize button is the one step after Save. No Test
     button: a keyless handshake is always 401 (see runConnTest). */
  figma: [
    DESC_FIELD,
    { k: "exposeResources", label: tk("Expose resources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("Expose prompts"), bool: true, def: true },
    AUTOSTART_EAGER,
  ],
  /* The zai-vision type: the GLM vision tools (@z_ai/mcp-server) compiled straight into the
     binary — the port replaced the Node child a proc def used to spawn, so an idle instance
     costs nothing. The key is ALWAYS a ${...} reference (the server refuses a literal); mode
     picks the official endpoint, baseUrl overrides it for a self-hosted GLM. No Test
     button: every call spends real tokens (see runConnTest). */
  "zai-vision": [
    DESC_FIELD,
    {
      k: "apiKey", label: tk("API key"), ph: "${Z_AI_API_KEY}",
      hint: tk("A ${ENV_VAR} or ${secret://name} reference — the server refuses a literal key. Store it once on the Plugins page or in the sealed env store."),
    },
    {
      k: "mode", label: tk("Mode"), half: true, ph: "ZHIPU",
      hint: tk("ZHIPU = open.bigmodel.cn (default) · ZAI = api.z.ai international."),
    },
    {
      k: "model", label: tk("Model"), half: true, ph: "glm-5.3-flash",
      hint: tk("Optional — the default is the model the deployed upstream used."),
    },
    {
      k: "baseUrl", label: tk("Base URL override"), ph: tk("optional — a self-hosted GLM endpoint"),
      hint: tk("Leave empty to use the mode's official endpoint."),
    },
    {
      k: "proxy", label: tk("Proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("Route THIS MCP's requests through an HTTP(S) proxy — for an endpoint this machine cannot reach directly."),
    },
    { k: "timeoutMs", label: tk("Timeout (ms)"), num: true, half: true, ph: "300000", hint: tk("Vision generations are slow; the upstream default is 300 s.") },
    { k: "exposeResources", label: tk("Expose resources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("Expose prompts"), bool: true, def: true },
    AUTOSTART_EAGER,
  ],
  rest: [
    DESC_FIELD,
    { k: "baseUrl", label: tk("Base URL"), ph: "https://api.github.com" },
    {
      k: "headers", label: tk("Headers (NAME=VALUE per line)"), area: true, kv: true,
      ph: "Authorization=Bearer ${MY_API_KEY}",
      hint: tk("Prefer a reference — ${secret://name} (store it once on the Plugins page) or ${ENV_VAR} — so neither this panel nor managed.json ever holds the value."),
    },
    {
      k: "proxy", label: tk("Proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("Route THIS MCP's requests through an HTTP(S) proxy — for an API this machine cannot reach directly. Other MCPs are not affected."),
    },
    {
      k: "tools", label: tk("Tool declarations (JSON)"), area: true, json: true,
      ph: '[{"name":"get_repo","description":"Get a public GitHub repo.","input":{"owner":{"type":"string","required":true},"repo":{"type":"string","required":true}},"request":{"method":"GET","path":"/repos/{{owner}}/{{repo}}"},"pick":["full_name","description","stargazers_count"]}]',
      hint: tk("The request block is the vendor's own example with {{arg}} in the slots you want the model to fill. Note {{arg}} for tool arguments — ${VAR} means an environment variable."),
    },
    { k: "timeoutMs", label: tk("Timeout (ms)"), num: true, half: true, ph: "30000" },
    AUTOSTART_EAGER,
  ],
};
const TYPE_LABELS                         = {
  proc: tk("proc — spawn a command and proxy it"),
  mysql: tk("mysql — in-process driver"),
  redis: tk("redis — in-process driver"),
  pg: tk("postgres — in-process driver"),
  http: tk("http — proxy a remote MCP endpoint"),
  figma: tk("figma — Figma's official remote MCP (OAuth fully managed, authorize once)"),
  "zai-vision": tk("zai-vision — Zhipu GLM vision tools, compiled in (replaces the Node child)"),
  rest: tk("rest — declare tools over a plain HTTP API"),
};

function envToText(env                                           )         {
  return env ? Object.keys(env).map((k) => { return k + "=" + env[k]; }).join("\n") : "";
}
function envToObj(text                           )                         {
  const o                         = {};
  String(text || "").split(/\r?\n/).forEach((line) => {
    line = line.trim();
    if (!line || line.startsWith("#")) return;
    const i = line.indexOf("=");
    if (i < 0) return;
    o[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  });
  return o;
}

/** Render one field as a NODE (docs/37 R5). `p` prefixes element ids so the Add sheet and the
 *  inline editor can coexist. The label, hint, placeholder and value are text nodes and
 *  properties now - the esc() discipline this file carried is structural instead. */
function fieldNode(spec           , val         , p        )              {
  const id = p + spec.k;
  const hint = spec.hint ? h("div", { class: "hint" }, tr(spec.hint)) : null;
  if (spec.bool) {
    const on = val === undefined ? !!spec.def : !!val && val !== "false";
    return h("div", { class: "fld" },
      h("label", { class: "check" },
        h("input", { type: "checkbox", id: id, checked: on }),
        tr(spec.label)),
      hint);
  }
  /* `json` fields hold an authored structure (a rest MCP's tool declarations) rather than a value or a
     KEY=VALUE map, so they round-trip as pretty-printed JSON. */
  const v = val == null ? "" : spec.json ? JSON.stringify(val, null, 2) : (typeof val === "object" ? envToText(val                          ) : String(val));
  // A password field is pre-filled with the mask sentinel, and the server keeps the stored secret
  // only while that sentinel comes back untouched. Password-manager autofill silently replacing it
  // would overwrite the real credential on save, with nothing to distinguish that from an edit.
  const guard = /pass|secret|token|key|url|credential/i.test(spec.k);
  const areaProps = {
    id: id,
    placeholder: spec.ph ? tr(spec.ph) : undefined,
    autocomplete: guard ? ("off"         ) : undefined,
    spellcheck: guard ? false : undefined,
  };
  const body = spec.area
    ? h("textarea", areaProps, v)
    : h("input", Object.assign({ type: "text"         , value: v }, areaProps));
  return h("div", { class: "fld" },
    h("label", { class: "field" }, h("span", null, tr(spec.label)), body),
    hint);
}

/** Lay a type's fields out as nodes, pairing the ones marked `half` into two columns. */
function fieldsNode(type        , vals                                            , p        )           {
  const specs = TYPE_FIELDS[type] || [];
  const out           = [];
  let i = 0;
  while (i < specs.length) {
    if (specs[i].half && specs[i + 1] && specs[i + 1].half) {
      out.push(h("div", { class: "two" },
        fieldNode(specs[i], vals && vals[specs[i].k], p),
        fieldNode(specs[i + 1], vals && vals[specs[i + 1].k], p)));
      i += 2;
    } else {
      out.push(fieldNode(specs[i], vals && vals[specs[i].k], p));
      i += 1;
    }
  }
  return out;
}

/** The form's auth checkbox is the def's auth string (docs/24 D1): checked sends "oauth",
 *  unchecked removes the key — which is how OAuth is switched back off. An empty client name
 *  never travels; the server applies its provider default. Both submit paths run the body
 *  through this before the server sees it, exactly like autostart -> lazy. */
function translateOauth(body                         )                          {
  if (body.auth === true) body.auth = "oauth";
  else delete body.auth;
  if (!body.oauthClientName) delete body.oauthClientName;
  return body;
}

/* --- pg url <-> fields (docs/30) ------------------------------------------------------------------ */
/* The def keeps ONE url string (the engine, the mask/unmask machinery and DIRECT_FIELDS all
 * read it); the form edits the pieces. The pair below is the only place the split happens,
 * used on open AND on every submit path. Two things must survive verbatim:
 *   - ${...} refs (a ${secret://name} password contains ':' and '/', which the url grammar
 *     would otherwise claim), so refs are lifted out to placeholders before the split;
 *   - the mask sentinel in a url password — the server restores it from the stored def.
 * Params render one k=v per line; the url keeps them &-joined. */
const PG_PARTS = ["host", "port", "user", "password", "database", "params"];
function parsePgUrl(url                           )                    {
  const refs           = [];
  const s = String(url || "").replace(/\$\{[^}]*\}/g, (r) => {
    refs.push(r);
    return "\u0001" + (refs.length - 1) + "\u0001";
  });
  /* The lifted placeholders are \u0001 + digits + \u0001 — none of :@/? — so the url grammar
   * below can stay plain; the refs ride through whatever slot they sit in. */
  const m = /^postgres(?:ql)?:\/\/(?:([^:@/]*)(?::([^@]*))?@)?([^:/?]*)(?::(\d+))?\/([^?]*)(?:\?(.*))?$/.exec(s);
  if (!m) return null;
  const back = (v        )         => {
    return v.replace(/\u0001(\d+)\u0001/g, (_        , i        )         => { return refs[+i] || ""; });
  };
  return {
    host: back(m[3] || ""),
    port: back(m[4] || ""),
    user: back(m[1] || ""),
    password: back(m[2] || ""),
    database: back(m[5] || ""),
    params: back(m[6] || "").replace(/&/g, "\n"),
  };
}
function pgUrlFrom(p                                                                                                 )         {
  const auth = p.user ? p.user + (p.password ? ":" + p.password : "") + "@" : "";
  const port = p.port ? ":" + p.port : "";
  const q = p.params ? "?" + String(p.params).split(/\r?\n/).map((l) => { return l.trim(); })
    .filter(Boolean).join("&") : "";
  return "postgresql://" + auth + (p.host || "") + port + "/" + (p.database || "") + q;
}
/** The submit-path half: parts -> one url, on every body the server sees. Precedence: any
 *  filled part field wins (the operator is decomposing — a whole-value ${...} url ref or an
 *  unparseable string gave the raw field, and typing into the parts is the choice to replace
 *  it); with no part filled, __pgRaw passes through untouched — a whole-value ref stays a
 *  ref, old experience beats lost data. */
function translatePg(type        , body                         )                          {
  if (type !== "pg") { delete body.__pgRaw; return body; }
  const hasParts = PG_PARTS.some((k) => { return body[k] !== undefined; });
  if (!hasParts && body.__pgRaw != null) {
    body.url = String(body.__pgRaw);
  } else {
    body.url = pgUrlFrom({
      host: body.host          , port: body.port          , user: body.user          ,
      password: body.password          , database: body.database          , params: body.params          ,
    });
  }
  PG_PARTS.forEach((k) => { delete body[k]; });
  delete body.__pgRaw;
  return body;
}

function readFields(type        , p        )                          {
  const o                          = {};
  /* docs/30: the unparseable-url fallback renders its own textarea (#e-pgraw / #a-pgraw);
     every submit path reads through readFields, so the raw value boards here like any field. */
  if (type === "pg") { const raw = $                  (p + "pgraw"); if (raw) o.__pgRaw = raw.value; }
  (TYPE_FIELDS[type] || []).forEach((f) => {
    const node = $                  (p + f.k);
    if (!node) return;
    if (f.bool) { o[f.k] = node.checked; return; }
    const raw = node.value;
    if (f.kv) { o[f.k] = envToObj(raw); return; }
    /* Malformed JSON is sent through as the raw string on purpose: the server answers with what is
       wrong with it, which is a better message than anything this form could invent. */
    if (f.json) { try { o[f.k] = JSON.parse(raw); } catch (e) { o[f.k] = raw; } return; }
    if (f.num) { if (raw !== "") o[f.k] = Number(raw); return; }
    if (raw !== "") o[f.k] = raw;
  });
  return o;
}

export { AUTOSTART_EAGER, AUTOSTART_PROC, DESC_FIELD, TESTABLE_TYPES, TYPE_FIELDS, TYPE_LABELS, envToObj, envToText, fieldNode, fieldsNode, parsePgUrl, pgUrlFrom, readFields, translateOauth, translatePg };