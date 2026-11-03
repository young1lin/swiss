/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
import { h } from "../h.js";

/** An immutable result page. Selection carries server row ids, never a visible row number. */
export function resultTable(o                                                                                                                                                                               )              {
  const table = h("table", { class: "result-table", aria: { label: o.label } },
    h("thead", null, h("tr", null, h("th"), o.columns.map(c => h("th", { scope: "col" }, c)))));
  const body = h("tbody");
  o.rows.forEach((row, i) => {
    const id = o.ids[i];
    const check = h("input", { type: "checkbox", checked: o.selected.has(id), aria: { label: o.selectLabel(id) } });
    check.onchange = ()       => { o.onSelect(id, check.checked); };
    const values = o.columns.map((column, at) => {
      const v          = Array.isArray(row) ? row[at] : row && typeof row === "object" ? (row                           )[column] : null;
      const text = v == null ? "∅" : typeof v === "string" ? v : JSON.stringify(v);
      return h("td", { title: text }, text);
    });
    body.appendChild(h("tr", { data: { rowId: id } }, h("td", null, check), values));
  });
  table.appendChild(body);
  return h("div", { class: "result-table-wrap" }, table);
}

;                                                                             

/** Declarative, non-executable chart data. The server owns aggregation, this owns geometry.
 * Bars, lines and areas use categories in the stored series order. A scatter plot instead uses
 * each point's real numeric x; repeated x values remain independent points. */
export function resultChart(o                                                                                                                                          )              {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 640 240");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", o.title);
  const scatter = o.kind === "scatter";
  const points = o.points.map(p => ({ ...p, numericX: p.x ?? (p.label.trim() ? Number(p.label) : NaN) }))
    .filter(p => Number.isFinite(p.value) && (!scatter || Number.isFinite(p.numericX)));
  const left = 56, right = 620, top = 20, bottom = 180;
  const low = Math.min(0, ...points.map(p => p.value));
  const high = Math.max(1, ...points.map(p => p.value));
  // Scaling before subtraction avoids overflow for opposite extreme finite values.
  const ratio = (v        , lo        , hi        )         => { const scale = Math.max(Math.abs(lo), Math.abs(hi), 1); return hi === lo ? 0.5 : (v / scale - lo / scale) / (hi / scale - lo / scale); };
  const y = (v        )         => bottom - ratio(v, low, high) * (bottom - top);
  const step = (right - left) / Math.max(1, points.length);
  const xLow = scatter ? Math.min(...points.map(p => p.numericX)) : 0;
  const xHigh = scatter ? Math.max(...points.map(p => p.numericX)) : 1;
  const xPad = xHigh === xLow ? Math.max(1, Math.abs(xLow) / 2) : 0;
  const x = (p                       , i        )         => scatter
    ? left + (xPad ? 0.5 : ratio(p.numericX, xLow, xHigh)) * (right - left)
    : left + (i + 0.5) * step;
  const node = (kind        , attrs                                 )             => {
    const n = document.createElementNS(NS, kind);
    Object.entries(attrs).forEach(([key, value]) => n.setAttribute(key, String(value)));
    svg.appendChild(n);
    return n;
  };
  const text = (value        , attrs                                 )             => {
    const n = node("text", attrs);
    n.textContent = value;
    return n;
  };
  const tick = (v        )         => Number(v.toPrecision(6)).toString();
  node("line", { x1: left, x2: right, y1: y(0), y2: y(0), class: "result-chart-axis" });
  node("line", { x1: left, x2: left, y1: top, y2: bottom, class: "result-chart-axis" });
  [low, high].forEach(v => {
    node("line", { x1: left - 4, x2: left, y1: y(v), y2: y(v), class: "result-chart-axis" });
    text(tick(v), { x: left - 8, y: y(v) + 4, "text-anchor": "end", class: "result-chart-label" });
  });
  if (o.xLabel) text(o.xLabel, { x: (left + right) / 2, y: 235, "text-anchor": "middle", class: "result-chart-label" });
  if (o.yLabel) text(o.yLabel, { x: left, y: 12, class: "result-chart-label" });
  if (!points.length) {
    text(o.emptyLabel ?? "", { x: (left + right) / 2, y: (top + bottom) / 2, "text-anchor": "middle", class: "result-chart-label" });
  }
  if (o.kind === "line" || o.kind === "area") {
    const coords = points.map((p, i) => `${x(p, i)},${y(p.value)}`).join(" ");
    if (o.kind === "area" && points.length) {
      node("polygon", { points: `${x(points[0], 0)},${y(0)} ${coords} ${x(points[points.length - 1], points.length - 1)},${y(0)}`, class: "result-chart-area" });
    }
    if (points.length > 1) node("polyline", { points: coords, class: "result-chart-line" });
  }
  points.forEach((p, i) => {
    const px = x(p, i);
    const shape = o.kind === "bar"
      ? node("rect", { x: px - step * 0.35, y: Math.min(y(p.value), y(0)), width: step * 0.7, height: Math.max(1, Math.abs(y(p.value) - y(0))), class: "result-chart-value" })
      : node("circle", { cx: px, cy: y(p.value), r: 3, class: "result-chart-value", "data-x": scatter ? p.numericX : p.label });
    const title = document.createElementNS(NS, "title");
    title.textContent = `${p.label}: ${p.value}`;
    shape.appendChild(title);
    if (points.length <= 12 || i === 0 || i === points.length - 1) {
      const label = node("text", { x: px, y: 205, "text-anchor": "middle", class: "result-chart-label" });
      label.textContent = p.label.slice(0, 16);
      if (p.label.length > 16) {
        const full = document.createElementNS(NS, "title");
        full.textContent = p.label;
        label.appendChild(full);
      }
    }
  });
  return h("figure", { class: "result-chart" }, h("figcaption", null, o.title, o.aggregation ? h("span", { class: "result-chart-note" }, " · " + o.aggregation) : null), svg);
}
