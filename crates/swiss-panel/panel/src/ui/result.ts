/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
import { h } from "../h.js";

/** An immutable result page. Selection carries server row ids, never a visible row number. */
export function resultTable(o: { columns: string[]; rows: unknown[]; ids: string[]; selected: Set<string>; label: string; selectLabel: (id: string) => string; onSelect: (id: string, on: boolean) => void }): HTMLElement {
  const table = h("table", { class: "result-table", aria: { label: o.label } },
    h("thead", null, h("tr", null, h("th"), o.columns.map(c => h("th", { scope: "col" }, c)))));
  const body = h("tbody");
  o.rows.forEach((row, i) => {
    const id = o.ids[i];
    const check = h("input", { type: "checkbox", checked: o.selected.has(id), aria: { label: o.selectLabel(id) } });
    check.onchange = (): void => { o.onSelect(id, check.checked); };
    const values = o.columns.map((column, at) => {
      const v: unknown = Array.isArray(row) ? row[at] : row && typeof row === "object" ? (row as Record<string, unknown>)[column] : null;
      const text = v == null ? "∅" : typeof v === "string" ? v : JSON.stringify(v);
      return h("td", { title: text }, text);
    });
    body.appendChild(h("tr", { data: { rowId: id } }, h("td", null, check), values));
  });
  table.appendChild(body);
  return h("div", { class: "result-table-wrap" }, table);
}

/** Declarative, non-executable chart data. The server owns aggregation, this owns geometry. */
export function resultChart(o: { title: string; kind: string; points: { label: string; value: number }[] }): HTMLElement {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 640 220");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", o.title);
  const points = o.points.filter(p => Number.isFinite(p.value));
  const low = Math.min(0, ...points.map(p => p.value));
  const high = Math.max(1, ...points.map(p => p.value));
  const y = (v: number): number => 180 - (v - low) / (high - low) * 160;
  const step = 600 / Math.max(1, points.length);
  const node = (kind: string, attrs: Record<string, string | number>): SVGElement => {
    const n = document.createElementNS(NS, kind);
    Object.entries(attrs).forEach(([key, value]) => n.setAttribute(key, String(value)));
    svg.appendChild(n);
    return n;
  };
  node("line", { x1: 20, x2: 620, y1: y(0), y2: y(0), class: "result-chart-axis" });
  if (o.kind === "line" || o.kind === "area") {
    const coords = points.map((p, i) => `${20 + (i + 0.5) * step},${y(p.value)}`).join(" ");
    if (o.kind === "area") node("polygon", { points: `20,${y(0)} ${coords} 620,${y(0)}`, class: "result-chart-area" });
    node("polyline", { points: coords, class: "result-chart-line" });
  }
  points.forEach((p, i) => {
    const x = 20 + (i + 0.5) * step;
    const shape = o.kind === "bar"
      ? node("rect", { x: x - step * 0.35, y: Math.min(y(p.value), y(0)), width: step * 0.7, height: Math.max(1, Math.abs(y(p.value) - y(0))), class: "result-chart-value" })
      : node("circle", { cx: x, cy: y(p.value), r: 3, class: "result-chart-value" });
    const title = document.createElementNS(NS, "title");
    title.textContent = `${p.label}: ${p.value}`;
    shape.appendChild(title);
    if (points.length <= 12 || i === 0 || i === points.length - 1) {
      const text = node("text", { x, y: 205, "text-anchor": "middle", class: "result-chart-label" });
      text.textContent = p.label.slice(0, 16);
    }
  });
  return h("figure", { class: "result-chart" }, h("figcaption", null, o.title), svg);
}
