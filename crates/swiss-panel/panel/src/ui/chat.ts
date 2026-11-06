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

/* The assistant pane (SPEC §panel.ui): a conversation docked at a page's end edge, beside the
 * work it acts on - the Data page's agent next to its console and results. Three parts, in a
 * column on the sidebar's surface:
 *
 *   head     the caption, then the tools: a picker (which agent), New, close;
 *   body     the conversation - a transcript() column, or a note while there is none - which
 *            scrolls on its own; chatFollow keeps its end in view while an answer streams;
 *   compose  one bordered input card: the words box, then permission controls at its start
 *            and model/send/stop actions at its end. It stays put however long the column grows.
 *            `queued` stands above the box: words sent while an answer streams, waiting their
 *            turn (a chatQueue(), which takes no room while empty).
 *
 * The pane sits after a grip({ end }), which sizes it and folds it away; --chat-pane-w is its
 * width before a drag. The caller wires the controls; the words box's own manners are the
 * pane's: it grows with its words up to a cap and scrolls past it (chatGrow), and with
 * `commands` a "/" at its start lists them above it to complete (chatCommands). `stats` is the
 * line under the actions: the conversation's account and its context ring (a chatStats()). */
import type { HChild } from "../h.js";
import { h } from "../h.js";
import { tr, trn } from "../i18n.js";
import { iconBtn } from "./button.js";
import { iconNode } from "./icon.js";

export interface ChatPaneOpts {
  id?: string;
  /** The pane's name: its caption and its accessible label. */
  cap: string;
  /** A picker and the current subject beside the caption, separate from head actions. */
  heading?: HChild;
  subject?: HChild;
  tools?: HChild[];
  body: HChild;
  input: HTMLTextAreaElement;
  /** What "/" at the box's start offers to complete (none: the box completes nothing). */
  commands?: ChatCommand[];
  /** Above the words box: what waits to be sent (a chatQueue()'s node, gone while empty). */
  queued?: HChild;
  /** Editable references actually included in the next send. */
  context?: HChild;
  /** The line under the words box: a note at its start, the actions at its end. */
  note?: HChild;
  /** Leading controls inside the input card, such as the current permission mode. */
  controls?: HChild[];
  actions: HChild[];
  /** Under the actions: the conversation's account (a chatStats()'s node, hidden while empty). */
  stats?: HChild;
}

export function chatPane(o: ChatPaneOpts): HTMLElement {
  chatGrow(o.input);
  const menu = o.commands && o.commands.length ? chatCommands(o.input, o.commands) : null;
  const body = h("div", { class: "chat-body" }, o.body);
  const latest = iconBtn("arrow-down", tr("ui.chat.toBottom"), { hidden: true });
  latest.classList.add("chat-jump");
  const pane = h("aside", { class: "chat-pane", id: o.id, aria: { label: o.cap } },
    h("div", { class: "chat-head" },
      h("div", { class: "chat-heading" },
        h("span", { class: "sec-cap", title: o.cap }, o.cap), o.heading,
        o.subject ? h("span", { class: "chat-subject" }, o.subject) : null),
      o.tools && o.tools.length ? h("span", { class: "chat-tools" }, o.tools) : null),
    body,
    h("div", { class: "chat-compose" },
      o.queued,
      o.context,
      h("div", { class: "chat-editor" }, o.input,
        h("div", { class: "chat-acts" }, o.controls,
          h("span", { class: "chat-note" }, o.note),
          o.actions)),
      o.stats,
      menu,
      latest));
  bindFollow(pane, body, latest);
  return pane;
}

/** One message waiting in a chatQueue: its words, and whether it was steered - handed to the
 *  live answer, which reads it before its next step. */
export interface ChatQueued {
  id: string;
  text: string;
  steered: boolean;
}

export interface ChatQueueOpts {
  id?: string;
  /** The words box's cap: an edit holds no more. */
  max?: number;
  /** ↑ on a row, moved to the front first: the host steers it now - into the live answer, or
   *  as the next ask when nothing streams - and marks it (steer) or takes it (remove). */
  onSteer(q: ChatQueued): void;
  /** The trash on a row, or an edit saved empty: it is gone; a steered one the host withdraws. */
  onDelete(q: ChatQueued): void;
  /** An edit ended, saved or not: what waited on it may go now. */
  onEdited?(): void;
}

export interface ChatQueue {
  node: HTMLElement;
  /** The rows in order, the next to go first. */
  items(): ChatQueued[];
  add(text: string): ChatQueued;
  /** Take a row off without a word to the host: the loop read it, or it went as an ask. */
  remove(id: string): ChatQueued | null;
  /** Mark a row steered, or back to waiting (the answer ended before reading it). */
  steer(id: string, on: boolean): void;
  /** The row the reader is editing (null: none). */
  editing(): ChatQueued | null;
}

let queueSeq = 0;

/** What waits to be sent while an answer streams (SPEC §panel.ui), as a card above the words
 *  box: a head naming how many, which folds the rows away, then one row each - its first line,
 *  and Edit, Delete and ↑ (steer it now) at its end. The rows go one per turn after a clean
 *  answer, in order; ↑ puts one first and hands it to the live answer, which reads it before
 *  its next step - a steered row shows so, dim, and Delete withdraws it. Edit turns the row
 *  into a box in place: Enter or leaving it saves, Escape keeps the words as they were, saved
 *  empty it is deleted. The card takes no room while empty. The host owns the sending: this
 *  holds the rows and says what the reader did. */
export function chatQueue(o: ChatQueueOpts): ChatQueue {
  const listId = "chat-queue-" + String(++queueSeq);
  const rows: Array<{ q: ChatQueued; row: HTMLElement }> = [];
  let open = true;
  let editingId: string | null = null;
  let seq = 0;
  const count = h("span", { class: "chat-queue-n" });
  const head = h("button", { type: "button", class: "chat-queue-head", aria: { expanded: "true", controls: listId } },
    iconNode("list"), count, iconNode(open ? "chevron-down" : "chevron-up"));
  const list = h("div", { class: "chat-queue-list", id: listId });
  const node = h("div", { class: "chat-queue", id: o.id, hidden: true }, head, list);
  head.onclick = (): void => {
    open = !open;
    list.hidden = !open;
    head.setAttribute("aria-expanded", String(open));
    head.lastElementChild?.replaceWith(iconNode(open ? "chevron-down" : "chevron-up"));
  };
  const paintHead = (): void => {
    node.hidden = !rows.length;
    count.textContent = trn(rows.length, "ui.chat.queued.one", "ui.chat.queued.other");
  };
  const find = (id: string): number => rows.findIndex((r) => r.q.id === id);
  const drop = (id: string): ChatQueued | null => {
    const at = find(id);
    if (at < 0) return null;
    const [r] = rows.splice(at, 1);
    r.row.remove();
    if (editingId === id) editingId = null;
    paintHead();
    return r.q;
  };
  const paint = (r: { q: ChatQueued; row: HTMLElement }): void => {
    const { q, row } = r;
    row.toggleAttribute("data-steered", q.steered);
    if (editingId === q.id) {
      const box = h("textarea", { class: "chat-queue-edit", rows: 1, maxLength: o.max, aria: { label: tr("ui.chat.queueEdit") } });
      box.value = q.text;
      const done = (save: boolean): void => {
        if (editingId !== q.id) return;
        editingId = null;
        const text = box.value.trim();
        if (save && !text) {
          drop(q.id);
          o.onDelete(q);
        } else {
          if (save) q.text = text;
          paint(r);
        }
        if (o.onEdited) o.onEdited();
      };
      box.addEventListener("keydown", (e: KeyboardEvent): void => {
        if (e.isComposing) return;
        // The keys stop here: never the page's Enter, never the panel's Escape.
        if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); e.stopPropagation(); done(true); }
        else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(false); }
      });
      box.addEventListener("blur", (): void => { done(true); });
      row.replaceChildren(box);
      chatGrow(box);
      box.focus();
      box.setSelectionRange(box.value.length, box.value.length);
      return;
    }
    const first = q.text.split("\n")[0];
    const edit = iconBtn("pencil", tr("ui.chat.queueEdit"), { ghost: true, hidden: q.steered });
    const del = iconBtn("trash", q.steered ? tr("ui.chat.queueWithdraw") : tr("ui.chat.queueDelete"), { ghost: true });
    const up = q.steered
      ? h("span", { class: "chat-queue-tag" }, tr("ui.chat.queueSteering"))
      : iconBtn("arrow-up", tr("ui.chat.queueSteer"), { ghost: true });
    edit.onclick = (): void => {
      if (editingId) return;
      editingId = q.id;
      paint(r);
    };
    del.onclick = (): void => { if (drop(q.id)) o.onDelete(q); };
    if (up instanceof HTMLButtonElement) {
      up.onclick = (): void => {
        const at = find(q.id);
        if (at < 0) return;
        // First in line: steered or sent, it is the one the reader picked as next.
        rows.unshift(rows.splice(at, 1)[0]);
        list.prepend(row);
        o.onSteer(q);
      };
    }
    row.replaceChildren(
      h("span", { class: "chat-queue-text", title: q.text }, first !== q.text ? first + " …" : first),
      edit, del, up);
  };
  return {
    node,
    items: (): ChatQueued[] => rows.map((r) => r.q),
    add: (text: string): ChatQueued => {
      const q: ChatQueued = { id: "q" + Date.now().toString(36) + "-" + String(++seq), text, steered: false };
      const r = { q, row: h("div", { class: "chat-queue-row", data: { queued: q.id } }) };
      rows.push(r);
      paint(r);
      list.appendChild(r.row);
      paintHead();
      return q;
    },
    remove: drop,
    steer: (id: string, on: boolean): void => {
      const at = find(id);
      if (at < 0 || rows[at].q.steered === on) return;
      rows[at].q.steered = on;
      paint(rows[at]);
    },
    editing: (): ChatQueued | null => {
      const at = editingId ? find(editingId) : -1;
      return at < 0 ? null : rows[at].q;
    },
  };
}

const following = new WeakMap<HTMLElement, (force: boolean) => void>();

/** Content growth does not give up the tail; reading does (DSH's policy, widened): a step up of
 *  any size, a scroll down that stops short of the end, a fold the reader opens. The button sits
 *  above the composer, outside the scrolling transcript. */
function bindFollow(pane: HTMLElement, body: HTMLElement, latest: HTMLButtonElement): void {
  const fromEnd = (): number => body.scrollHeight - body.clientHeight - body.scrollTop;
  const nearEnd = (): boolean => fromEnd() <= 24;
  let tail = true;
  let lastTop = body.scrollTop;
  let frame: number | null = null;
  const paint = (): void => {
    if (tail) body.scrollTop = body.scrollHeight;
    lastTop = body.scrollTop;
    latest.hidden = nearEnd();
  };
  const update = (force: boolean): void => {
    if (force) tail = true;
    paint();
    // Re-measure after the browser lays out markdown, folds and the growing composer.
    // A reading gesture before this frame still wins over the pending follow.
    if (frame == null && pane.isConnected) frame = requestAnimationFrame(() => {
      frame = null;
      if (pane.isConnected) paint();
    });
  };
  following.set(pane, update);
  latest.onclick = (): void => { update(true); };
  // A column with nowhere to go up to has nothing to give up: the wheel over a short one is idle.
  body.addEventListener("wheel", (e) => { if (e.deltaY < 0 && body.scrollTop > 0) tail = false; }, { passive: true });
  // Before the fold opens: the scrollbar it may bring resizes the body, and that must not carry
  // the row the reader just clicked up and out of view.
  body.addEventListener("click", (e) => {
    const fold = (e.target as Element).closest("summary")?.parentElement;
    if (fold instanceof HTMLDetailsElement && !fold.open) { tail = false; update(false); }
  });
  body.addEventListener("scroll", () => {
    // paint() records its own writes, so a move seen here is the reader's, or a shorter column
    // clamping to its end, which is the one way up that keeps the tail.
    const top = body.scrollTop;
    if (top < lastTop) tail = fromEnd() <= 1;
    else if (top > lastTop) tail = nearEnd();
    lastTop = top;
    latest.hidden = nearEnd();
  });
  if (typeof ResizeObserver !== "undefined") {
    const size = new ResizeObserver(() => {
      if (!pane.isConnected) { size.disconnect(); return; }
      update(false);
    });
    size.observe(body);
  }
}

/** Follow an answer's latest paint while the reader owns the tail; a new send forces it. */
export function chatFollow(pane: HTMLElement, force = false): void {
  const update = following.get(pane);
  if (update) { update(force); return; }
  const body = pane.querySelector<HTMLElement>(".chat-body");
  if (body) body.scrollTop = body.scrollHeight;
}

/** A command the words box completes: its name after a "/" at the box's start. */
export interface ChatCommand {
  /** The name, without the slash. */
  name: string;
  /** What may follow the name, as its row shows it: "[focus]". */
  arg?: string;
  /** One line: what it does. */
  hint: string;
}

/** What a box does after each write lands: refit, re-read the command (a box can carry both). */
const onWrite = new WeakMap<HTMLTextAreaElement, (() => void)[]>();

/** Run fn after every write to the box: typed, pasted, or set by the page (`box.value = ""`
 *  after a send, words handed back). The setter is the instance's own, so every existing write
 *  reaches it, as styleSelect's does for a select. */
function afterWrite(ta: HTMLTextAreaElement, fn: () => void): void {
  let fns = onWrite.get(ta);
  if (!fns) {
    const list: (() => void)[] = [];
    fns = list;
    onWrite.set(ta, list);
    ta.addEventListener("input", () => { list.forEach((f) => { f(); }); });
    // Read when a box is made, never at import: a suite on a stubbed DOM loads the library too.
    const own = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(ta) as object, "value") as { get(): string; set(v: string): void } | undefined;
    if (own) {
      Object.defineProperty(ta, "value", {
        get: () => own.get.call(ta),
        set: (v: string) => { own.set.call(ta, v); list.forEach((f) => { f(); }); },
        configurable: true,
      });
    }
  }
  fns.push(fn);
}

/** Run fn after every write to a chat's words box, typed or set by the page: for a pane whose
 *  controls follow the words, such as a round button that stops a streaming reply while the box
 *  is empty and sends (queues) once it holds words. */
export function chatAfterWrite(ta: HTMLTextAreaElement, fn: () => void): void {
  afterWrite(ta, fn);
}

/** The words box grows with its words: its rows are the floor, the CSS max-height the cap, and
 *  past the cap it scrolls. It refits on every write, on focus, and when its width changes (the
 *  same words wrap to another height); a box not laid out (a folded pane) keeps its rows. */
export function chatGrow(ta: HTMLTextAreaElement): void {
  if (ta.dataset.grow) return;
  ta.dataset.grow = "1";
  const fit = (): void => {
    ta.style.height = "auto";
    if (!ta.scrollHeight) { ta.style.height = ""; return; }
    ta.style.height = String(ta.scrollHeight + ta.offsetHeight - ta.clientHeight) + "px";
  };
  afterWrite(ta, fit);
  ta.addEventListener("focus", fit);
  if (typeof ResizeObserver === "undefined") return;
  let width = -1;
  new ResizeObserver(() => {
    // A width change only: the height fit sets fires the observer again.
    if (ta.clientWidth === width) return;
    width = ta.clientWidth;
    fit();
  }).observe(ta);
}

let cmdSeq = 0;

/** "/" at the start of the words box lists the commands whose name begins with what follows it
 *  (then those that hold it), above the box: the arrows walk them, Tab or Enter completes the
 *  one marked ("/name " - the box goes on to its argument), Escape closes the list until the
 *  words change, a click completes and leaves the caret in the box. The box is the combobox and
 *  keeps the focus (aria-activedescendant); Enter on a name typed whole is the page's: it sends.
 *  A key the list takes goes no further - not to the page's own Enter, not to the panel's
 *  Escape. Returns the list, for the caller to place above the box. */
export function chatCommands(ta: HTMLTextAreaElement, commands: ChatCommand[]): HTMLElement {
  const listId = "chat-cmds-" + String(++cmdSeq);
  const menu = h("div", { class: "chat-cmds", id: listId, role: "listbox", hidden: true, aria: { label: tr("ui.chat.commands") } });
  let shown: ChatCommand[] = [];
  let at = 0;
  // The words Escape closed the list on: it stays shut until they change.
  let dismissed: string | null = null;
  ta.setAttribute("aria-autocomplete", "list");
  ta.setAttribute("aria-controls", listId);
  ta.setAttribute("aria-expanded", "false");

  const mark = (): void => {
    Array.from(menu.children).forEach((r, i) => { r.setAttribute("aria-selected", String(i === at)); });
    const row = menu.children[at] as HTMLElement | undefined;
    if (!row) return;
    ta.setAttribute("aria-activedescendant", row.id);
    if (typeof row.scrollIntoView === "function") row.scrollIntoView({ block: "nearest" });
  };
  const hide = (): void => {
    shown = [];
    menu.hidden = true;
    menu.replaceChildren();
    ta.setAttribute("aria-expanded", "false");
    ta.removeAttribute("aria-activedescendant");
  };
  const complete = (c: ChatCommand): void => {
    ta.value = "/" + c.name + " ";
    ta.setSelectionRange(ta.value.length, ta.value.length);
    ta.focus();
  };
  const paint = (): void => {
    const v = ta.value;
    if (v !== dismissed) dismissed = null;
    if (dismissed !== null || document.activeElement !== ta || !v.startsWith("/") || /\s/.test(v)) { hide(); return; }
    const q = v.slice(1).toLowerCase();
    const named = commands.map((c) => ({ c, n: c.name.toLowerCase() }));
    const next = [...named.filter((x) => x.n.startsWith(q)), ...named.filter((x) => !x.n.startsWith(q) && x.n.includes(q))].map((x) => x.c);
    if (!next.length) { hide(); return; }
    const was = shown[at] as ChatCommand | undefined;
    shown = next;
    at = was ? Math.max(0, shown.indexOf(was)) : 0;
    menu.replaceChildren(...shown.map((c, i) => {
      const row = h("div", { class: "chat-cmd", id: listId + "-" + String(i), role: "option", aria: { selected: String(i === at) } },
        h("span", { class: "chat-cmd-name" }, "/" + c.name, c.arg ? h("span", { class: "chat-cmd-arg" }, " " + c.arg) : null),
        h("span", { class: "chat-cmd-hint", title: c.hint }, c.hint));
      // Mousedown, not click: the box keeps the focus, so its blur never shuts the list first.
      row.addEventListener("mousedown", (e) => { e.preventDefault(); complete(c); });
      row.addEventListener("mouseenter", () => { at = i; mark(); });
      return row;
    }));
    menu.hidden = false;
    ta.setAttribute("aria-expanded", "true");
    mark();
  };

  afterWrite(ta, paint);
  ta.addEventListener("focus", paint);
  ta.addEventListener("blur", hide);
  ta.addEventListener("keydown", (e) => {
    if (!shown.length || e.isComposing) return;
    const take = (): void => { e.preventDefault(); e.stopImmediatePropagation(); };
    const typedWhole = ta.value === "/" + shown[at].name;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      take();
      at = (at + (e.key === "ArrowDown" ? 1 : shown.length - 1)) % shown.length;
      mark();
    } else if ((e.key === "Tab" || (e.key === "Enter" && !typedWhole)) && !e.shiftKey) {
      take();
      complete(shown[at]);
    } else if (e.key === "Escape") {
      take();
      dismissed = ta.value;
      hide();
    }
  });
  return menu;
}
