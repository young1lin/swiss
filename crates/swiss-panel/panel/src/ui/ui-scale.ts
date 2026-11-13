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

/* --- SPEC §panel.scale: the interface size ----------------------------------------------------------
   A small screen at 1080p draws the panel's 14px text at a size the reader leans in to read. The
   browser's own zoom (Ctrl +) is one answer, but it zooms every page of the site and moves the
   panel's measurements into a second coordinate space. This is the panel's own: one factor,
   --ui-scale, multiplies every length the stylesheets write from 3px up (styles/base.css), so the
   type, the air, the rows and the columns grow together, and a rect a script measures is still
   the rect it positions by.

   The preference is per browser (localStorage swiss_scale, like the theme): it describes this
   screen. <html data-scale> holds the step, set before the first paint by both pages' inline
   scripts; no attribute is 100%. A length a script writes goes through scaledCss (a size that
   should follow the factor live, like a dragged pane) or scaled (a bound it compares against a
   measurement). */

/** Where the preference lives; absent is 100%. */
const SCALE_KEY = "swiss_scale";

/** The steps, in the order the picker lists them. styles/base.css has one block per step but 100. */
const UI_SCALES: string[] = ["90", "100", "110", "125", "150", "175"];

/** A stored or received value as a step; anything unknown is "100". The pages' pre-paint
 *  scripts repeat this to beat the first paint (a test runs them against it). */
function scaleStep(pref: string | null | undefined): string {
  return pref && UI_SCALES.includes(pref) ? pref : "100";
}

/** The step stored for this browser. */
function scalePref(): string {
  try { return scaleStep(localStorage.getItem(SCALE_KEY)); }
  catch (e) { return "100"; }
}

/** The factor in force on this page: 1.25 under data-scale="125", 1 without one. */
function uiScale(): number {
  // No page (a worker, a test's bare document stub): nothing is scaled.
  const root = typeof document === "undefined" ? null : document.documentElement;
  if (!root || typeof root.getAttribute !== "function") return 1;
  return Number(scaleStep(root.getAttribute("data-scale"))) / 100;
}

/** A length at the size in force, in px: the script's twin of calc(Npx * var(--ui-scale)), for a
 *  bound a measurement is compared with. */
function scaled(px: number): number {
  return px * uiScale();
}

/** A length as the stylesheets write it, for a style a script sets: it follows a later change of
 *  the size without the script painting again. */
function scaledCss(px: number): string {
  return "calc(" + px + "px * var(--ui-scale))";
}

/** Put a step on <html>. The pages that measure their space (the terminal, the tab strips, the
 *  rail's captions) remeasure on the resize event, as they do for Focus mode. */
function applyScale(step: string): void {
  const s = scaleStep(step);
  const root = document.documentElement;
  if (s === "100") root.removeAttribute("data-scale");
  else root.setAttribute("data-scale", s);
  if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") window.dispatchEvent(new Event("resize"));
}

/** Store a step for this browser and apply it. A blocked store still applies it to this page. */
function setScale(step: string): void {
  const s = scaleStep(step);
  try { if (s === "100") localStorage.removeItem(SCALE_KEY); else localStorage.setItem(SCALE_KEY, s); }
  catch (e) { /* the choice still applies to this page load */ }
  applyScale(s);
}

export { SCALE_KEY, UI_SCALES, applyScale, scalePref, scaleStep, scaled, scaledCss, setScale, uiScale };
