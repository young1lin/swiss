// Copyright 2026 young1lin
// SPDX-License-Identifier: Apache-2.0

/** Inspect the execution toolbox on demand, rather than maintaining a second description. */
import { fill, h } from "../h.js";
import { tk, tr } from "../i18n.js";
import { formFold, hint, proseText, valueBlock } from "../ui/index.js";
import type { LlmRuntime, LlmToolInfo } from "../types/llm.js";
import { call, toolSummary } from "./llm-core.js";
import { jsonBlock } from "./llm-history.js";

interface Definition { context: string; name: string; description: string; parameters: unknown }

const CONTEXT_LABELS: Record<string, string> = {
  sql: tk("llm.inspect.context.sql"), mongo: tk("llm.inspect.context.mongo"), redis: tk("llm.inspect.context.redis"),
  interactive: tk("llm.inspect.context.interactive"), memory: tk("llm.inspect.context.memory"), lead: tk("llm.inspect.context.lead"),
};

export function toolDetails(runtime: LlmRuntime, tool: LlmToolInfo, options: { kind?: string; agent?: string; cwd?: () => string; named?: boolean } = {}): HTMLElement {
  const body = h("div");
  const fold = formFold({ summary: options.named ? [h("code", null, tool.name), " · ", toolSummary(runtime, tool)] : toolSummary(runtime, tool) }, body) as HTMLDetailsElement;
  fold.dataset.toolDefinition = tool.name;
  let opened = false;
  let request = 0;
  fold.ontoggle = async (): Promise<void> => {
    if (opened === fold.open) return;
    opened = fold.open;
    const serial = ++request;
    if (!opened) return;
    if (runtime === "claude" || runtime === "pi") {
      fill(body, hint(tr("llm.inspect.external")));
      return;
    }
    fill(body, hint(tr("llm.inspect.loading")));
    const query = new URLSearchParams({ runtime, tool: tool.name });
    if (options.kind) query.set("kind", options.kind);
    if (options.agent) query.set("agent", options.agent);
    const cwd = options.cwd?.().trim();
    if (cwd) query.set("cwd", cwd);
    const result = await call<{ definitions: Definition[] }>("GET", "/api/llm/tools?" + query.toString());
    if (serial !== request) return;
    if (!result.data) { fill(body, hint(result.err, { bad: true })); return; }
    fill(body,
      runtime === "builtin" ? hint(tr("llm.inspect.contextHint")) : null,
      result.data.definitions.map((d) => valueBlock({ label: tr(CONTEXT_LABELS[d.context] || d.context) },
        valueBlock({ label: tr("llm.inspect.modelName"), text: d.name }),
        valueBlock({ label: tr("llm.inspect.description") }, proseText(d.description)),
        valueBlock({ label: tr("llm.inspect.parameters") }, jsonBlock(d.parameters)))));
  };
  return fold;
}
