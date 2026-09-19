import { beforeAll, describe, expect, it, vi } from "vitest";

// @vitest-environment happy-dom

/* docs/31 — the Logs tab's server-side search box. Pure markup: what is pinned is that the input
   renders with the live needle, the empty state names the needle, and the unfiltered wording is
   untouched when there is no needle. logs.js itself touches no DOM at import time; its two
   imports do (the sidebar graph wires buttons), so both are mocked — the page-registry idiom
   from admin-panel: mock the leaves, keep the module under test real. */
vi.mock("../src/util.js", () => ({
  esc: (s: string) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"),
  icon: () => "",
  iconNode: () => document.createElement("span"),
  state: {},
}));
vi.mock("../src/sidebar.js", () => ({ rowOf: () => null }));

let logs: typeof import("../src/logs.js");

beforeAll(async () => {
  logs = await import("../src/logs.js");
});

function stub(over: Record<string, unknown> = {}) {
  return {
    name: "redis", calls: [], callsPage: 0, callsMore: false,
    callsFull: {}, callsOpen: {}, stderr: "", callsQ: "",
    ...over,
  } as never;
}

/* docs/37 R5: logsBodyNode paints a real tree — assertions read the painted DOM. */
function paint(over: Record<string, unknown> = {}): HTMLElement {
  const host = document.createElement("div");
  host.append(...[logs.logsBodyNode(stub(over) as never)].flat().filter((n): n is Node => n != null));
  return host;
}

describe("docs/31: logs search box", () => {
  it("renders the search input with the live needle", () => {
    const input = paint({ callsQ: "GET" }).querySelector<HTMLInputElement>("#callsQ")!;
    expect(input.value).toBe("GET");
    expect(input.getAttribute("placeholder")).toBe("Search calls");
  });

  it("the empty state names the needle instead of pretending nothing was ever called", () => {
    const text = paint({ callsQ: "GET" }).textContent!;
    expect(text).toContain("No calls matching");
    expect(text).toContain("GET");
    expect(text).not.toContain("No calls yet");
  });

  it("without a needle the original empty state stands", () => {
    const text = paint().textContent!;
    expect(text).toContain("No calls yet");
    expect(text).not.toContain("No calls matching");
  });

  it("docs/32 B4: the toolbar action is the ellipsis menu — no standing Clear button", () => {
    const host = paint({ callsQ: "x" });
    expect(host.querySelector("#clMenu")!.getAttribute("aria-label")).toBe("More log actions");
    expect(host.querySelector("#callsClear")).toBeNull();
    expect(host.textContent).not.toContain("Clear logs");
  });
});
