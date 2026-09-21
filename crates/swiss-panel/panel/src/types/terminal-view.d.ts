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

/* The terminal view's own ambient shapes (docs/36 D6/D7): the per-session model
   views/terminal.ts wires and the vendored-package bundle its load() caches. The
   surface augmentations this file once carried (Window.webkitAudioContext, the
   Function-property memo) are retired - docs/37 M3 - in favour of local casts and
   module-scoped state in views/terminal.ts. */

import type { JsonSchemaNode } from "./api.js";

/** One wired session. The literals in select()/openSession() build the core fields; the
 *  rest join at their first real value (wireTerminal, connect, paintJump) and are
 *  optional for exactly that reason - the literals predate them. */
export interface TermModel {
  id: string;
  target: string;
  label: string;
  status: string;
  attempt: number;
  userClosed: boolean;
  gone: boolean;
  sentCols: number;
  sentRows: number;
  /* OSC 0/2 title vs the manual rename - tabLabel() reads both (docs/22 consensus 1). */
  shellTitle: string | null;
  customTitle: string | null;
  bell: boolean;
  pinned: boolean;
  unseen: number;
  /* Keystrokes typed before the first socket opens ride exactly, then never again. */
  buffered: string[] | null;
  sync2026: number;
  repaint2026: boolean;
  suppressSelect: boolean;
  search: XtermSearchAddon | null;
  term?: XtermTerminal | null;
  fit?: XtermFitAddon | null;
  holder?: HTMLDivElement | null;
  overlay?: { show(text: string, ms?: number): void; dispose(): void } | null;
  gl?: XtermWebglAddon | null;
  jump?: HTMLButtonElement | null;
  ws?: WebSocket | null;
  toBottom?: (() => void) | null;
}

/** The five vendored constructors load() caches for the module's lifetime. */
export interface TerminalPackages {
  Terminal: XtermTerminalCtor;
  FitAddon: XtermFitAddonCtor;
  Unicode11Addon: XtermUnicode11AddonCtor;
  WebLinksAddon: XtermWebLinksAddonCtor;
  WebglAddon: new () => XtermWebglAddon;
}

/* The two fit-addon members the resize/open paths read (exporting this file made it a
   module, so its interface no longer ambient-merges with types/vendor.d.ts's
   XtermFitAddon - the members the panel touches are declared here instead).
   proposeDimensions may answer nothing when the terminal is not measurable yet. */
export interface XtermFitAddon {
  fit(): void;
  proposeDimensions(): { cols: number; rows: number } | undefined;
}

/* --- the Local shell settings sheet (views/terminal-settings.ts) -------------------------------- */

/** The local block of the terminal plugin's config, as the settings sheet reads it. */
export interface TerminalLocalCfg {
  enabled?: boolean;
  shell?: string;
  shells?: { program: string; label: string }[];
}

/** The terminal plugin's config row - the keys src/plugins/terminal.rs config_schema
 *  declares (docs/14 section 6): the local block the settings sheet edits, and the remote
 *  session limits it leaves alone. Every field is optional on the wire; the plugin defaults
 *  what is absent. */
export interface TerminalPluginConfig {
  local?: TerminalLocalCfg | null;
  allowedTargets?: string[];
  maxSessions?: number;
  maxSessionsPerTarget?: number;
  idleTimeoutMinutes?: number;
  graceSeconds?: number;
  stallSeconds?: number;
  recording?: boolean;
}

/** GET /api/plugins/terminal/config as the settings sheet reads it (host/api.rs get_config):
 *  the row, the revision the save PUT echoes back, and the descriptor's JSON schema, whose
 *  properties.local.properties.enabled.description is the switch's reason line. The walk
 *  sits in a try because an older schema can lack a link of that chain. */
export interface TerminalPluginConfigResponse {
  revision: number;
  config: TerminalPluginConfig;
  schema: JsonSchemaNode | null;
}
