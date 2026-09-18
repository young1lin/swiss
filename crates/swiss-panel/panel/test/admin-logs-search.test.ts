import { beforeAll, describe, expect, it, vi } from "vitest";

/* docs/31 — the Logs tab's server-side search box. Pure markup: what is pinned is that the input
   renders with the live needle, the empty state names the needle, and the unfiltered wording is
   untouched when there is no needle. logs.js itself touches no DOM at import time; its two
   imports do (the sidebar graph wires buttons), so both are mocked — the page-registry idiom
   from admin-panel: mock the leaves, keep the module under test real. */
vi.mock("../src/util.js", () => ({
  esc: (s: string) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"),
  icon: () => "",
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

describe("docs/31: logs search box", () => {
  it("renders the search input with the live needle", () => {
    const html = logs.logsBody(stub({ callsQ: "GET" }));
    const input = html.match(/<input id="callsQ"[^>]*>/)![0];
    expect(input).toContain('value="GET"');
    expect(input).toContain('placeholder="Search calls"');
  });

  it("the empty state names the needle instead of pretending nothing was ever called", () => {
    const html = logs.logsBody(stub({ callsQ: "GET" }));
    expect(html).toContain("No calls matching");
    expect(html).toContain("GET");
    expect(html).not.toContain("No calls yet");
  });

  it("without a needle the original empty state stands", () => {
    const html = logs.logsBody(stub());
    expect(html).toContain("No calls yet");
    expect(html).not.toContain("No calls matching");
  });

  it("docs/32 B4: the toolbar action is the ellipsis menu — no standing Clear button", () => {
    const html = logs.logsBody(stub({ callsQ: "x" }));
    expect(html).toContain('id="clMenu"');
    expect(html).toContain('aria-label="More log actions"');
    expect(html).not.toContain("callsClear");
  });
});
