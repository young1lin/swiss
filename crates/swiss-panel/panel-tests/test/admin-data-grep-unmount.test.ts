import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/* The DOM-stub technique the panel suites use (admin-data-grep.test.ts), with RECORDING
   timers so the 300ms grep debounce can be fired deterministically — the bug is what its
   callback does after the view unmounted, not 300ms later. */
type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false, checked: false,
    value: "", textContent: "", className: "", id: "", title: "", type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) { n.children.push(c); return c; },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute() {}, getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => { n.children = []; } });
  return n;
};
const byId: Record<string, Stub> = {};
const timers: { fn: () => void; ms: number }[] = [];
const origSet = globalThis.setTimeout;
(globalThis as any).setTimeout = ((fn: () => void, ms: number) => {
  timers.push({ fn, ms });
  return timers.length as any;
}) as any;
Object.assign(globalThis, {
  document: {
    documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
    activeElement: null,
    createElement: (t: string) => el(t), createTextNode: (s: string) => ({ text: s }),
    getElementById: (id: string) => (byId[id] ||= el()),
    querySelector: () => null, querySelectorAll: () => [],
    addEventListener: () => {}, removeEventListener: () => {},
  },
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const here = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js");
const view = await import(pathToFileURL(join(here, "data-view.js")).href) as {
  renderDbView: () => void;
};
const util = await import(pathToFileURL(join(here, "util.js")).href) as {
  state: { db: Record<string, any> | null };
};
// A snapshot of the factory-built state: a test that nulls state.db (the unmount) must be
// able to give the next test a fresh one, the way loadDbView does on every entry.
const freshDb = JSON.parse(JSON.stringify(util.state.db)) as Record<string, any>;

describe("the table-list grep debounce vs an unmounted view (docs/22 closeout audit)", () => {
  it("the 300ms callback firing after the view unmounted is a silent no-op, not a TypeError", () => {
    view.renderDbView();
    expect(util.state.db, "the view mounted its state").toBeTruthy();
    util.state.db!.conns = [{ name: "c", dialect: "mysql" }];
    util.state.db!.conn = "c";
    const grep = byId.dbGrep;
    expect(grep.oninput, "the grep box is wired").toBeTruthy();
    grep.value = "abc";
    grep.oninput.call(grep);
    const fired = timers.filter((t) => t.ms === 300);
    expect(fired.length, "the input scheduled its 300ms debounce").toBeGreaterThan(0);
    const cb = fired[fired.length - 1].fn;
    // The unmount frees state.db; the pending debounce must survive that without throwing.
    util.state.db = null;
    expect(() => cb(), "the late callback is a no-op").not.toThrow();
  });

  it("with the view still mounted the same callback still applies the grep", () => {
    util.state.db = JSON.parse(JSON.stringify(freshDb));
    view.renderDbView();
    util.state.db!.conns = [{ name: "c", dialect: "mysql" }];
    util.state.db!.conn = "c";
    const grep = byId.dbGrep;
    grep.value = "xyz";
    grep.oninput.call(grep);
    const cb = timers[timers.length - 1].fn;
    expect(() => cb()).not.toThrow();
    expect(util.state.db!.grep, "the debounce still applies the grep while mounted").toBe("xyz");
  });
});
