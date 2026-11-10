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

/* The Add sheet's templates (SPEC §process.panel): a known program's whole launch, so the
 * reader picks HOW it starts instead of working out which executable ends up holding the port.
 *
 * dsh is an npm-installed Node CLI: `dsh` resolves to dsh.cmd, which runs node.exe, and
 * node.exe is what listens. A form left to derive the image from the program ("dsh") read the
 * running app as a conflict with itself (2026-11-10). `node` matches node.exe on Windows and
 * node on Unix (SPEC §process.apps image).
 *
 * The port is the reader's: 3080 is only where it starts, since another process may hold it.
 * A template is a launch ON a port - `--port` in its argv is the port the row declares, so the
 * form rebuilds the argv as Ports changes and the two can never split. An emptied Ports is a
 * launch on no declared port: no `--port` (the program picks its own) and no image, since an
 * image only names who may hold a declared port. */
import { tk } from "../../i18n.js";
import type { AppEntry } from "./types.js";

export interface AppTemplate {
  /** The option's label, an i18n key. */
  label: string;
  /** The one quiet line under the select saying what the template decided, an i18n key. */
  why: string;
  /** The port it fills while Ports is empty. */
  port: number;
  /** What it fills on a port: name, program, args, ports and image. Nothing else is touched.
   *  null is no declared port: the launch without `--port`, ports or image. */
  app(port: number | null): AppEntry;
}

function dsh(...flags: string[]): (port: number | null) => AppEntry {
  return (port) => port == null
    ? { name: "dsh", start: { program: "dsh", args: ["web", ...flags] } }
    : {
      name: "dsh",
      start: { program: "dsh", args: ["web", ...flags, "--port", String(port)] },
      ports: [port],
      image: "node",
    };
}

/* Option order is the select's order. */
export const APP_TEMPLATES: Record<string, AppTemplate> = {
  "dsh-web": { label: tk("process.template.dshWeb"), why: tk("process.template.dshWebWhy"), port: 3080, app: dsh() },
  "dsh-web-no-open": { label: tk("process.template.dshWebNoOpen"), why: tk("process.template.dshWebNoOpenWhy"), port: 3080, app: dsh("--no-open") },
};

/* The one port a Ports text names, or null for none, several or a bad one. */
export function singlePort(text: string): number | null {
  const parts = text.split(",").map((s) => s.trim()).filter(Boolean);
  if (parts.length !== 1 || !/^\d+$/.test(parts[0])) return null;
  const n = Number(parts[0]);
  return n >= 1 && n <= 65535 ? n : null;
}
