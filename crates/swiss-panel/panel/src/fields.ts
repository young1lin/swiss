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

import type { FieldSpec, PgUrlParts } from "./types/dom.js";
import { $ } from "./util.js";
import { tk, tr } from "./i18n.js";
import { h } from "./h.js";
import type { HChild } from "./h.js";
import { checkField, field, pair } from "./ui/index.js";

/* --- field schemas ---------------------------------------------------------------------------- */
/* `description` leads every type: it is what the MCP client is told this endpoint is for, and with
   several instances of the same engine behind identical tool sets, it is the only thing that says
   which application's data is on the other end. */
const DESC_FIELD: FieldSpec = {
  k: "description", label: tk("fields.description"),
  ph: tk("fields.whatMcpEG"),
  hint: tk("fields.describesMcpItselfSent"),
};
/* "Start automatically at boot" — the user-facing side of the registry's `lazy` (inverted). A proc
   defaults OFF (its child is the one expensive idle thing here, so it starts on first request and
   is reaped when idle); every other type defaults ON. readFields returns it as `autostart`;
   submitAdd/saveEdit translate it to `lazy` before the server sees it. */
const AUTOSTART_PROC: FieldSpec = {
  k: "autostart", label: tk("fields.startAutomaticallyBoot"), bool: true, def: false,
  hint: tk("fields.offDefaultFirstClient"),
};
const AUTOSTART_EAGER: FieldSpec = {
  k: "autostart", label: tk("fields.startAutomaticallyBoot"), bool: true, def: true,
  hint: tk("fields.offIdleBootFirst"),
};
const TESTABLE_TYPES: string[] = ["mysql", "mariadb", "redis", "pg", "http", "rest"];
const TYPE_FIELDS: Record<string, FieldSpec[]> = {
  proc: [
    DESC_FIELD,
    { k: "command", label: tk("fields.command"), ph: "npx -y @modelcontextprotocol/server-git   ·   uvx mcp-server-git" },
    { k: "cwd", label: tk("fields.workingDirectory"), ph: tk("fields.optional") },
    { k: "env", label: tk("fields.environmentKeyValueLine"), area: true, kv: true, ph: "GIT_REPO=D:\\dev\\project" },
    { k: "exposeResources", label: tk("fields.exposeResources"), bool: true, def: true, hint: tk("fields.turnOffChildPublishes") },
    { k: "exposePrompts", label: tk("fields.exposePrompts"), bool: true, def: true },
    { k: "timeoutMs", label: tk("fields.callTimeoutMs"), num: true, ph: "180000", hint: tk("fields.howLongOneTool") },
    AUTOSTART_PROC,
  ],
  mysql: [
    DESC_FIELD,
    { k: "host", label: tk("fields.host"), half: true }, { k: "port", label: tk("fields.port"), num: true, half: true },
    { k: "user", label: tk("fields.user"), half: true }, { k: "password", label: tk("fields.password"), half: true },
    { k: "database", label: tk("fields.database"), half: true }, { k: "timezone", label: tk("fields.timezone"), half: true, ph: "Z" },
    { k: "maxRows", label: tk("fields.defaultRowLimit"), num: true, half: true, ph: "200" },
    AUTOSTART_EAGER,
  ],
  // SPEC §mcp.panel: MariaDB speaks the MySQL wire protocol — this form is the mysql one verbatim
  // (the server maps the type to the same engine); only the type string and the seal differ.
  mariadb: [
    DESC_FIELD,
    { k: "host", label: tk("fields.host"), half: true }, { k: "port", label: tk("fields.port"), num: true, half: true },
    { k: "user", label: tk("fields.user"), half: true }, { k: "password", label: tk("fields.password"), half: true },
    { k: "database", label: tk("fields.database"), half: true }, { k: "timezone", label: tk("fields.timezone"), half: true, ph: "Z" },
    { k: "maxRows", label: tk("fields.defaultRowLimit"), num: true, half: true, ph: "200" },
    AUTOSTART_EAGER,
  ],
  redis: [
    DESC_FIELD,
    { k: "host", label: tk("fields.host"), half: true }, { k: "port", label: tk("fields.port"), num: true, half: true },
    { k: "password", label: tk("fields.password"), half: true }, { k: "db", label: tk("fields.dbIndex"), num: true, half: true },
    { k: "allowDestructive", label: tk("fields.allowFlushallFlushdb"), bool: true },
    { k: "allowEval", label: tk("fields.allowLuaEvalFcall"), bool: true, hint: tk("fields.scriptOpaqueEveryOther") },
    AUTOSTART_EAGER,
  ],
  // SPEC §mcp.panel: the def keeps ONE url string; the form edits the pieces. parse/serialize below,
  // the same pair on open and on save, so an edit touches exactly the field it meant to.
  pg: [
    DESC_FIELD,
    { k: "host", label: tk("fields.host"), half: true }, { k: "port", label: tk("fields.port"), num: true, half: true },
    { k: "user", label: tk("fields.user"), half: true },
    {
      k: "password", label: tk("fields.password"), half: true,
      hint: tk("fields.literalEnvVarSecret"),
    },
    { k: "database", label: tk("fields.database"), half: true }, { k: "maxRows", label: tk("fields.defaultRowLimit"), num: true, half: true, ph: "200" },
    { k: "params", label: tk("fields.optionsKVLine"), area: true, ph: "sslmode=disable" },
    AUTOSTART_EAGER,
  ],
  http: [
    DESC_FIELD,
    { k: "url", label: tk("fields.endpointUrl"), area: true, ph: "https://mcp.context7.com/mcp" },
    {
      k: "headers", label: tk("fields.headersNameValueLine"), area: true, kv: true,
      ph: "Authorization=Bearer ${CONTEXT7_API_KEY}",
      hint: tk("fields.whereRemotesApiKey"),
    },
    /* OAuth (SPEC §mcp.oauth): the checkbox is the def's auth string — checked sends "oauth", and the
       gateway then owns Authorization end to end (register, consent, refresh). The headers
       field above must not carry Authorization when this is on; the server refuses the pair. */
    {
      k: "auth", label: tk("fields.oauthAuthorizationGatewayOwns"), bool: true, def: false,
      hint: tk("fields.remoteMcpsBehindOauth"),
    },
    {
      k: "oauthClientName", label: tk("fields.oauthClientName"), ph: tk("fields.emptyClaudeCode"),
      hint: tk("fields.figmasRegistrationAcceptsOnly"),
    },
    {
      k: "proxy", label: tk("fields.proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("fields.routeMcpsRequestsThrough"),
    },
    { k: "exposeResources", label: tk("fields.exposeResources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("fields.exposePrompts"), bool: true, def: true },
    AUTOSTART_EAGER,
  ],
  /* The figma type (SPEC §mcp.figma): the form is a description and nothing else. The endpoint
     and the OAuth mode are the type's to decide — the server refuses a url or auth key on a
     figma def — and the detail view's Authorize button is the one step after Save. No Test
     button: a keyless handshake is always 401 (see runConnTest). */
  figma: [
    DESC_FIELD,
    { k: "exposeResources", label: tk("fields.exposeResources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("fields.exposePrompts"), bool: true, def: true },
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
      k: "apiKey", label: tk("fields.apiKey"), ph: "${Z_AI_API_KEY}",
      hint: tk("fields.envVarSecretName"),
    },
    {
      k: "mode", label: tk("fields.mode"), half: true, ph: "ZHIPU",
      hint: tk("fields.zhipuOpenBigmodelCn"),
    },
    {
      k: "model", label: tk("fields.model"), half: true, ph: "glm-5.3-flash",
      hint: tk("fields.optionalDefaultModelDeployed"),
    },
    {
      k: "baseUrl", label: tk("fields.baseUrlOverride"), ph: tk("fields.optionalSelfHostedGlm"),
      hint: tk("fields.leaveEmptyUseModes"),
    },
    {
      k: "proxy", label: tk("fields.proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("fields.routeMcpSRequestsThroughHttp"),
    },
    { k: "timeoutMs", label: tk("fields.timeoutMs"), num: true, half: true, ph: "300000", hint: tk("fields.visionGenerationsSlowUpstream") },
    { k: "exposeResources", label: tk("fields.exposeResources"), bool: true, def: true },
    { k: "exposePrompts", label: tk("fields.exposePrompts"), bool: true, def: true },
    AUTOSTART_EAGER,
  ],
  rest: [
    DESC_FIELD,
    { k: "baseUrl", label: tk("fields.baseUrl"), ph: "https://api.github.com" },
    {
      k: "headers", label: tk("fields.headersNameValueLine"), area: true, kv: true,
      ph: "Authorization=Bearer ${MY_API_KEY}",
      hint: tk("fields.preferReferenceSecretName"),
    },
    {
      k: "proxy", label: tk("fields.proxy"), ph: "http://127.0.0.1:7890 or ${MY_PROXY}",
      hint: tk("fields.routeMcpsRequestsThrough2"),
    },
    {
      k: "tools", label: tk("fields.toolDeclarationsJson"), area: true, json: true,
      ph: '[{"name":"get_repo","description":"Get a public GitHub repo.","input":{"owner":{"type":"string","required":true},"repo":{"type":"string","required":true}},"request":{"method":"GET","path":"/repos/{{owner}}/{{repo}}"},"pick":["full_name","description","stargazers_count"]}]',
      hint: tk("fields.requestBlockVendorsOwn"),
    },
    { k: "timeoutMs", label: tk("fields.timeoutMs"), num: true, half: true, ph: "30000" },
    AUTOSTART_EAGER,
  ],
};
const TYPE_LABELS: Record<string, string> = {
  proc: tk("fields.procSpawnCommandProxy"),
  mysql: tk("fields.mysqlProcessDriver"),
  redis: tk("fields.redisProcessDriver"),
  pg: tk("fields.postgresProcessDriver"),
  http: tk("fields.httpProxyRemoteMcp"),
  figma: tk("fields.figmaFigmasOfficialRemote"),
  "zai-vision": tk("fields.zaiVisionZhipuGlm"),
  rest: tk("fields.restDeclareToolsOver"),
};

function envToText(env: Record<string, string> | null | undefined): string {
  return env ? Object.keys(env).map((k) => { return k + "=" + env[k]; }).join("\n") : "";
}
function envToObj(text: string | null | undefined): Record<string, string> {
  const o: Record<string, string> = {};
  String(text || "").split(/\r?\n/).forEach((line) => {
    line = line.trim();
    if (!line || line.startsWith("#")) return;
    const i = line.indexOf("=");
    if (i < 0) return;
    o[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  });
  return o;
}

/** Render one field as a NODE (SPEC §panel.toolchain). `p` prefixes element ids so the Add sheet and the
 *  inline editor can coexist. The label, hint, placeholder and value are text nodes and
 *  properties now - the esc() discipline this file carried is structural instead. */
function fieldNode(spec: FieldSpec, val: unknown, p: string): HTMLElement {
  const id = p + spec.k;
  const help = spec.hint ? tr(spec.hint) : null;
  if (spec.bool) {
    const on = val === undefined ? !!spec.def : !!val && val !== "false";
    return checkField({ label: tr(spec.label), hint: help, control: h("input", { type: "checkbox", id: id, checked: on }) as HTMLInputElement });
  }
  /* `json` fields hold an authored structure (a rest MCP's tool declarations) rather than a value or a
     KEY=VALUE map, so they round-trip as pretty-printed JSON. */
  const v = val == null ? "" : spec.json ? JSON.stringify(val, null, 2) : (typeof val === "object" ? envToText(val as Record<string, string>) : String(val));
  // A password field is pre-filled with the mask sentinel, and the server keeps the stored secret
  // only while that sentinel comes back untouched. Password-manager autofill silently replacing it
  // would overwrite the real credential on save, with nothing to distinguish that from an edit.
  const guard = /pass|secret|token|key|url|credential/i.test(spec.k);
  const areaProps = {
    id: id,
    placeholder: spec.ph ? tr(spec.ph) : undefined,
    autocomplete: guard ? ("off" as const) : undefined,
    spellcheck: guard ? false : undefined,
  };
  const body = spec.area
    ? h("textarea", areaProps, v)
    : h("input", Object.assign({ type: "text" as const, value: v }, areaProps));
  return field({ label: tr(spec.label), control: body, hint: help });
}

/** Lay a type's fields out as nodes, pairing the ones marked `half` into two columns. */
function fieldsNode(type: string, vals: Record<string, unknown> | null | undefined, p: string): HChild[] {
  const specs = TYPE_FIELDS[type] || [];
  const out: HChild[] = [];
  let i = 0;
  while (i < specs.length) {
    if (specs[i].half && specs[i + 1] && specs[i + 1].half) {
      out.push(pair(
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

/** The form's auth checkbox is the def's auth string (SPEC §mcp.oauth): checked sends "oauth",
 *  unchecked removes the key — which is how OAuth is switched back off. An empty client name
 *  never travels; the server applies its provider default. Both submit paths run the body
 *  through this before the server sees it, exactly like autostart -> lazy. */
function translateOauth(body: Record<string, unknown>): Record<string, unknown> {
  if (body.auth === true) body.auth = "oauth";
  else delete body.auth;
  if (!body.oauthClientName) delete body.oauthClientName;
  return body;
}

/* --- pg url <-> fields (SPEC §mcp.panel) ------------------------------------------------------------------ */
/* The def keeps ONE url string (the engine, the mask/unmask machinery and DIRECT_FIELDS all
 * read it); the form edits the pieces. The pair below is the only place the split happens,
 * used on open AND on every submit path. Two things must survive verbatim:
 *   - ${...} refs (a ${secret://name} password contains ':' and '/', which the url grammar
 *     would otherwise claim), so refs are lifted out to placeholders before the split;
 *   - the mask sentinel in a url password — the server restores it from the stored def.
 * Params render one k=v per line; the url keeps them &-joined. */
const PG_PARTS = ["host", "port", "user", "password", "database", "params"];
function parsePgUrl(url: string | null | undefined): PgUrlParts | null {
  const refs: string[] = [];
  const s = String(url || "").replace(/\$\{[^}]*\}/g, (r) => {
    refs.push(r);
    return "\u0001" + (refs.length - 1) + "\u0001";
  });
  /* The lifted placeholders are \u0001 + digits + \u0001 — none of :@/? — so the url grammar
   * below can stay plain; the refs ride through whatever slot they sit in. */
  const m = /^postgres(?:ql)?:\/\/(?:([^:@/]*)(?::([^@]*))?@)?([^:/?]*)(?::(\d+))?\/([^?]*)(?:\?(.*))?$/.exec(s);
  if (!m) return null;
  const back = (v: string): string => {
    return v.replace(/\u0001(\d+)\u0001/g, (_: string, i: string): string => { return refs[+i] || ""; });
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
function pgUrlFrom(p: Partial<Record<"host" | "port" | "user" | "password" | "database" | "params", string | number>>): string {
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
function translatePg(type: string, body: Record<string, unknown>): Record<string, unknown> {
  if (type !== "pg") { delete body.__pgRaw; return body; }
  const hasParts = PG_PARTS.some((k) => { return body[k] !== undefined; });
  if (!hasParts && body.__pgRaw != null) {
    body.url = String(body.__pgRaw);
  } else {
    body.url = pgUrlFrom({
      host: body.host as string, port: body.port as string, user: body.user as string,
      password: body.password as string, database: body.database as string, params: body.params as string,
    });
  }
  PG_PARTS.forEach((k) => { delete body[k]; });
  delete body.__pgRaw;
  return body;
}

function readFields(type: string, p: string): Record<string, unknown> {
  const o: Record<string, unknown> = {};
  /* SPEC §mcp.panel: the unparseable-url fallback renders its own textarea (#e-pgraw / #a-pgraw);
     every submit path reads through readFields, so the raw value boards here like any field. */
  if (type === "pg") { const raw = $<HTMLInputElement>(p + "pgraw"); if (raw) o.__pgRaw = raw.value; }
  (TYPE_FIELDS[type] || []).forEach((f) => {
    const node = $<HTMLInputElement>(p + f.k);
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