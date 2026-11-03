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

/* A conversation's account (SPEC §panel.ui), one quiet line centred under the assistant pane's
 * words box (chatPane's `stats`), dsh's composer dock - three pills, each opening its card:
 *
 *   activity  how many turns the reader took, how many steps the model ran, its output speed;
 *             its card, Session statistics, the model's time, the tools' time, the average wait
 *             for a first token and the output speed;
 *   tokens    what the conversation used, and the share the provider's cache served; its card,
 *             Token usage, the total exactly, then the hit rate, input never cached, read from
 *             the cache, written to it (when any) and output, each exactly;
 *   context   a ring that fills as the next request nears the agent's budget, its percent
 *             beside it; its card what fills it - the system prompt, the tool definitions and
 *             the messages, on a bar and a row each, under a note the caller gives.
 *
 * One card is open at a time, above the line over its pill; Escape or a press outside closes
 * it. The caller keeps the counts and hands them over whole (set); the line hides while there
 * is nothing to say. The ring reads tokens against the budget's fixed token `limit`: the count
 * the provider gave for the last round, request and answer (`used`), else the request's bytes
 * through `perByte` (the provider's last token count over that request's bytes), marked "~";
 * without a limit, bytes against the byte budget, as KiB. */
import { fill, h,             } from "../h.js";
import { tr, trn } from "../i18n.js";
import { iconNode } from "./icon.js";

/** What the next request carries against the budget, in bytes: the system prompt and the tool
 *  definitions apart, the messages the rest. */
                              
                
              
                 
                
                                                                                           
                 
                                                                                            
                                                                                             
                
                                                     
                   
 

/** A conversation's counts so far. Tokens as the provider counted them: input never read from
 *  its cache, read from it, written to it, and output. Times in ms. */
                            
                
                
                   
                    
                     
                    
                                                             
                
                 
                                                                                             
                 
                    
                                                                                         
                                                                                      
                   
                       
                              
 

                            
                    
                                                                             
                                 
 

                                          

/** The ring's radius in its 16-unit box, and its length. */
const RING_R = 6;
const RING_C = 2 * Math.PI * RING_R;
/** From these shares of the budget the ring warns, then says it is nearly full. */
const WARN_AT = 0.75;
const FULL_AT = 0.9;

/** A count the way the line reads one: 549, 11.6K, 107K, 12.4M. */
function fmtCount(n        )         {
  const v = Math.max(0, Math.round(n));
  if (v < 1000) return String(v);
  const [unit, by] = v < 1e6 ? ["K", 1e3] : v < 1e9 ? ["M", 1e6] : ["B", 1e9];
  const x = v / by;
  return (x < 100 ? String(Math.round(x * 10) / 10) : String(Math.round(x))) + unit;
}

/** A count whole, its thousands apart: 13,096,027. */
function fmtExact(n        )         {
  return String(Math.max(0, Math.round(n))).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

function fmtKiB(bytes        )         {
  const k = bytes / 1024;
  return tr("ui.chat.kib", { n: k < 10 ? String(Math.round(k * 10) / 10) : String(Math.round(k)) });
}

/** A time the way dsh reads one: 45.2s under a minute, 2m42s from there on. */
function fmtDuration(ms        )         {
  const s = ms / 1000;
  if (s < 60) return tr("ui.chat.secs", { n: String(Math.round(s * 10) / 10) });
  const whole = Math.round(s);
  return tr("ui.chat.mins", { m: String(Math.floor(whole / 60)), s: String(whole % 60) });
}

/** The cache's share of the prompt as a percent: whole, unless rounding would claim a full hit
 *  the counts deny - then as few decimals as keep it under 100. Null with no prompt. */
function hitPct(read        , prompt        )                {
  if (prompt <= 0) return null;
  if (read >= prompt) return "100";
  const share = read / prompt * 100;
  for (let d = 0; d < 4; d++) {
    const s = share.toFixed(d);
    if (Number(s) < 100) return s;
  }
  return "99.999";
}

/** Output tokens a second: over the time after each round's first token where the rounds said
 *  it, else over the rounds' whole time. Null with neither. */
function speedOf(u           )                {
  if (u.decodeMs > 0 && u.decodeTokens > 0) return u.decodeTokens / (u.decodeMs / 1000);
  if (u.llmMs > 0 && u.tokensOut > 0) return u.tokensOut / (u.llmMs / 1000);
  return null;
}

/** How full the context reads: tokens against the fixed limit - the provider's count, else the
 *  bytes as tokens, marked (~) - or, with neither, bytes against the byte budget. The parts
 *  (system prompt, tool definitions, messages) share the reading as they share the bytes. */
function gauge(c             )                                                                                                {
  const used = c.used ?? (c.perByte ? c.bytes * c.perByte : null);
  if (c.limit && used != null) {
    const mark = c.used != null ? "" : "~";
    return {
      share: used / c.limit, estimated: c.used == null,
      used: mark + fmtCount(used),
      cap: fmtCount(c.limit),
      part: (bytes) => mark + fmtCount(c.bytes > 0 ? used * bytes / c.bytes : 0),
    };
  }
  return { share: c.cap > 0 ? c.bytes / c.cap : 0, estimated: false, used: fmtKiB(c.bytes), cap: fmtKiB(c.cap), part: fmtKiB };
}

/** The ring: a track and the share filled, from twelve o'clock. */
function ring()                                                {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("class", "chat-ring");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("aria-hidden", "true");
  const circle = (cls        )                   => {
    const c = document.createElementNS(NS, "circle");
    c.setAttribute("class", cls);
    c.setAttribute("cx", "8");
    c.setAttribute("cy", "8");
    c.setAttribute("r", String(RING_R));
    c.setAttribute("stroke-width", "2");
    return c;
  };
  const arc = circle("chat-ring-fill");
  arc.setAttribute("transform", "rotate(-90 8 8)");
  svg.appendChild(circle("chat-ring-track"));
  svg.appendChild(arc);
  return { svg, arc };
}

export function chatStats(o                                 = {})            {
  const popId = (o.id || "chat") + "-pop";
  const pill = (stat        )                    =>
    h("button", { type: "button", class: "chat-stat", data: { stat }, aria: { haspopup: "dialog", expanded: "false", controls: popId } })                     ;
  const activity = pill("activity");
  const tokens = pill("tokens");
  const { svg, arc } = ring();
  const pct = h("span", { class: "chat-ctx-pct" });
  const button = h("button", { type: "button", class: "chat-ctx", aria: { haspopup: "dialog", expanded: "false", controls: popId } }, svg, pct)                     ;
  const pop = h("div", { class: "chat-pop", id: popId, role: "dialog", hidden: true });
  const node = h("div", { class: "chat-stats", id: o.id, hidden: true }, activity, tokens, button, pop);
  const triggers                                  = { activity, tokens, ctx: button };
  let shown                   = null;
  let open              = null;

  /** A pill's words: one reading, or two a quiet dot apart; the whole is its name and title. */
  const read = (el                   , icon        , first        , second               )       => {
    const whole = second == null ? first : first + " · " + second;
    el.replaceChildren(iconNode(icon), h("span", { class: "chat-stat-label" }, first,
      second == null ? null : h("span", { class: "chat-stat-sep", aria: { hidden: "true" } }, "·"), second));
    el.setAttribute("aria-label", whole);
    el.title = whole;
  };
  const counts = (u           )         =>
    trn(u.turns, "ui.chat.turns.one", "ui.chat.turns.other", { n: String(u.turns) }) + " " +
    trn(u.steps, "ui.chat.steps.one", "ui.chat.steps.other", { n: String(u.steps) });
  const perSec = (speed        )         => tr("ui.chat.tokPerSec", { n: String(Math.round(speed)) });

  /** A card's head - its name (after an icon) and the reading it sums up, a rule under them -
   *  then its body. */
  const card = (title        , icon               , value        , ...body          )       => {
    pop.setAttribute("aria-label", title);
    fill(pop,
      h("div", { class: "chat-pop-title" },
        h("span", { class: "chat-pop-label" }, icon ? iconNode(icon) : null, title),
        h("span", { class: "chat-pop-value" }, value)),
      h("div", { class: "chat-pop-rule", aria: { hidden: "true" } }),
      ...body);
  };
  const row = (key        , label        , value        )              =>
    h("div", { data: { row: key } }, h("dt", null, label), h("dd", null, value));

  const paintPop = ()       => {
    const u = shown;
    if (!u || !open) { pop.replaceChildren(); return; }
    if (open === "activity") {
      const speed = speedOf(u);
      card(tr("ui.chat.statsTitle"), "gauge", counts(u), h("dl", { class: "chat-pop-rows" },
        u.llmMs > 0 ? row("llm", tr("ui.chat.llmTime"), fmtDuration(u.llmMs)) : null,
        u.toolMs > 0 ? row("tool", tr("ui.chat.toolTime"), fmtDuration(u.toolMs)) : null,
        u.ttftSteps > 0 ? row("ttft", tr("ui.chat.ttft"), fmtDuration(u.ttftMs / u.ttftSteps)) : null,
        speed != null ? row("speed", tr("ui.chat.speed"), perSec(speed)) : null));
      return;
    }
    if (open === "tokens") {
      const exact = (n        )         => tr("ui.chat.tokens", { n: fmtExact(n) });
      const prompt = u.tokensIn + u.cacheRead + u.cacheWrite;
      const hit = hitPct(u.cacheRead, prompt);
      card(tr("ui.chat.usageTitle"), "database", exact(prompt + u.tokensOut), h("dl", { class: "chat-pop-rows" },
        hit != null ? row("hit", tr("ui.chat.hitRow"), hit + "%") : null,
        row("in", tr("ui.chat.inputRow"), exact(u.tokensIn)),
        row("read", tr("ui.chat.readRow"), exact(u.cacheRead)),
        // A conversation that never wrote to the cache has no such row.
        u.cacheWrite ? row("write", tr("ui.chat.writeRow"), exact(u.cacheWrite)) : null,
        row("out", tr("ui.chat.outputRow"), exact(u.tokensOut))));
      return;
    }
    const c = u.context;
    if (!c) { pop.replaceChildren(); return; }
    const g = gauge(c);
    const messages = Math.max(0, c.bytes - c.system - c.tools);
    const parts                             = [
      ["system", tr("ui.chat.ctxSystem"), c.system],
      ["tools", tr("ui.chat.ctxTools"), c.tools],
      ["messages", tr("ui.chat.ctxMessages"), messages],
    ];
    // The bar's length is the reading's share; its parts split it as the bytes do.
    const filled = Math.min(1, g.share) / Math.max(c.bytes, 1);
    const bar = h("div", { class: "chat-ctx-bar", aria: { hidden: "true" } }, parts.map(([part, , bytes]) => {
      const seg = h("span", { class: "chat-ctx-seg", data: { part } });
      seg.style.width = (bytes * filled * 100).toFixed(2) + "%";
      return seg;
    }));
    card(tr("ui.chat.ctxHead", { pct: pct.textContent || "" }), null, g.used + " / " + g.cap,
      bar,
      h("dl", { class: "chat-pop-rows" }, parts.map(([part, label, bytes]) =>
        row(part, [h("i", { class: "chat-ctx-key", data: { part } }), label], g.part(bytes)))),
      o.note ? h("p", { class: "chat-pop-note" }, o.note) : null);
  };

  /** Over its pill, inside the line: centred on the pill, held within the line's width. */
  const place = ()       => {
    if (!open) return;
    const line = node.getBoundingClientRect();
    const at = triggers[open].getBoundingClientRect();
    const w = pop.offsetWidth || 0;
    const left = Math.min(Math.max(0, at.left + at.width / 2 - w / 2 - line.left), Math.max(0, line.width - w));
    if (Number.isFinite(left)) pop.style.left = Math.round(left) + "px";
  };

  const onKey = (e               )       => {
    if (e.key !== "Escape" || !open) return;
    // The panel's own Escape (a sheet's, the pane's) never hears the one that closes this.
    e.preventDefault();
    e.stopPropagation();
    const back = triggers[open];
    close();
    back.focus();
  };
  const onPress = (e       )       => {
    const t = e.target;
    // A press on another pill switches the card (its click), so only one outside them closes it.
    if (t instanceof Node && (pop.contains(t) || Object.values(triggers).some((b) => b.contains(t)))) return;
    close();
  };
  const show = (which      )       => {
    if (open) triggers[open].setAttribute("aria-expanded", "false");
    else {
      // On the window, capturing: ahead of every Escape the page itself listens for.
      window.addEventListener("keydown", onKey, true);
      document.addEventListener("pointerdown", onPress, true);
    }
    open = which;
    triggers[which].setAttribute("aria-expanded", "true");
    paintPop();
    pop.hidden = false;
    place();
  };
  function close()       {
    if (!open) return;
    triggers[open].setAttribute("aria-expanded", "false");
    open = null;
    pop.hidden = true;
    window.removeEventListener("keydown", onKey, true);
    document.removeEventListener("pointerdown", onPress, true);
  }
  for (const which of Object.keys(triggers)          ) {
    triggers[which].onclick = ()       => { if (open === which) close(); else show(which); };
  }

  const set = (u                  )       => {
    shown = u;
    node.hidden = !u || (!u.steps && !u.context);
    if (!u) { close(); return; }
    const prompt = u.tokensIn + u.cacheRead + u.cacheWrite;
    const total = prompt + u.tokensOut;
    activity.hidden = !u.steps;
    tokens.hidden = !u.steps || total <= 0;
    if (u.steps) {
      const speed = speedOf(u);
      read(activity, "gauge", counts(u), speed == null ? null : perSec(speed));
      // With no time counted the card would be empty: the pill only reads.
      activity.disabled = u.llmMs <= 0 && u.toolMs <= 0 && u.ttftSteps <= 0;
      const hit = hitPct(u.cacheRead, prompt);
      read(tokens, "database", tr("ui.chat.tokens", { n: fmtCount(total) }), hit == null ? null : tr("ui.chat.cacheHit", { pct: hit }));
    }
    const c = u.context;
    button.hidden = !c;
    if (c) {
      const g = gauge(c), share = g.share;
      pct.textContent = (g.estimated ? "~" : "") + Math.round(share * 100) + "%";
      arc.setAttribute("stroke-dasharray", (RING_C * Math.min(1, share)).toFixed(2) + " " + RING_C.toFixed(2));
      button.dataset.level = share >= FULL_AT ? "full" : share >= WARN_AT ? "warn" : "";
      const label = tr("ui.chat.ctxLabel", { pct: pct.textContent });
      button.setAttribute("aria-label", label);
      button.title = label;
    }
    if (!open) return;
    if (triggers[open].hidden || triggers[open].disabled) close();
    else { paintPop(); place(); }
  };
  return { node, set };
}
