import { beforeEach, describe, expect, it, vi } from "vitest";

const env = vi.hoisted(() => ({
  els: new Map<string, Record<string, unknown>>(),
  apiJson: vi.fn(),
  closeSheet: vi.fn(),
  state: { view: "system" },
}));

vi.mock("../src/util.js", () => ({
  $: (id: string) => env.els.get(id) || null,
  apiJson: env.apiJson,
  emptyHtml: (o: { title: string; hint: string }) =>
    '<div class="empty"><div><h2>' + o.title + "</h2><p>" + o.hint + "</p></div></div>",
  icon: () => "",
  state: env.state,
}));

vi.mock("../src/add-sheet.js", () => ({
  closeSheet: env.closeSheet,
}));

function el(over: Record<string, unknown> = {}) {
  return {
    hidden: true,
    innerHTML: "",
    textContent: "",
    disabled: false,
    onclick: null,
    focus: vi.fn(),
    ...over,
  } as Record<string, unknown>;
}

describe("Settings / System", () => {
  beforeEach(() => {
    env.els.clear();
    env.els.set("pane", el());
    env.els.set("sheet", el());
    env.els.set("system-quit", el());
    env.els.set("quit-cancel", el());
    env.els.set("quit-confirm", el());
    env.state.view = "system";
    env.apiJson.mockReset();
    env.closeSheet.mockReset();
  });

  it("places the low-frequency destructive action in a dedicated Runtime section", async () => {
    const system = await import("../src/views/system.js");
    await system.mount();
    const html = String(env.els.get("pane")!.innerHTML);
    expect(html).toContain("Control this running swiss process");
    expect(html).toContain("Runtime");
    expect(html).toContain('class="btn danger" id="system-quit"');
    expect(html).toContain("Quit swiss");
    expect(typeof env.els.get("system-quit")!.onclick).toBe("function");
  });

  it("opens a visible, explicit confirmation sheet before stopping anything", async () => {
    const system = await import("../src/views/system.js");
    await system.mount();
    (env.els.get("system-quit")!.onclick as () => void)();
    expect(env.apiJson).not.toHaveBeenCalled();
    expect(env.els.get("sheet")!.hidden).toBe(false);
    expect(String(env.els.get("sheet")!.innerHTML)).toContain('role="dialog"');
    expect(String(env.els.get("sheet")!.innerHTML)).toContain("disconnects every MCP client");
    expect(typeof env.els.get("quit-cancel")!.onclick).toBe("function");
    expect(typeof env.els.get("quit-confirm")!.onclick).toBe("function");
    (env.els.get("quit-cancel")!.onclick as () => void)();
    expect(env.closeSheet).toHaveBeenCalledOnce();
  });

  it("restores the confirmation when the shutdown request is refused", async () => {
    const system = await import("../src/views/system.js");
    env.els.get("pane")!.innerHTML = "unchanged";
    env.apiJson.mockResolvedValue(null);
    await system.requestQuit();
    expect(env.els.get("quit-confirm")!.disabled).toBe(false);
    expect(env.els.get("quit-confirm")!.textContent).toBe("Quit swiss");
    expect(env.closeSheet).not.toHaveBeenCalled();
    expect(env.els.get("pane")!.innerHTML).toBe("unchanged");
  });

  it("POSTs the existing graceful shutdown action and leaves a final page state", async () => {
    const system = await import("../src/views/system.js");
    env.apiJson.mockResolvedValue({ ok: true, stopping: true });
    await system.requestQuit();
    expect(env.apiJson).toHaveBeenCalledWith("/api/shutdown", { method: "POST" });
    expect(env.closeSheet).toHaveBeenCalledOnce();
    expect(String(env.els.get("pane")!.innerHTML)).toContain("swiss is stopping");
    expect(String(env.els.get("pane")!.innerHTML)).toContain("close this tab");
  });

  it("does not overwrite a different page after a late shutdown response", async () => {
    const system = await import("../src/views/system.js");
    env.els.get("pane")!.innerHTML = "new page";
    env.state.view = "plugins";
    env.apiJson.mockResolvedValue({ ok: true, stopping: true });
    await system.requestQuit();
    expect(env.closeSheet).toHaveBeenCalledOnce();
    expect(env.els.get("pane")!.innerHTML).toBe("new page");
  });
});
