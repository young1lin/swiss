/*
 * Copyright 2026 The swiss authors
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

/* The xterm.js type face the PANEL actually calls (docs/36 D7) - ambient globals so each
   vendored shim's index.d.ts stays a one-liner, exactly like the shims themselves. Only
   members the panel touches are declared: this is a mirror of js/vendor/..., the upstream
   @xterm/xterm types stay out of the tree. Add a member here the day the panel first
   calls it - not before (a vendored surface nobody reads is a second spec to keep true). */

/** One terminal buffer viewport - the scroll/pin math reads active only. */
interface XtermBuffer {
  viewportY: number;
  baseY: number;
  type: "normal" | "alternate";
}

/** The CSI handler spec term.parser.registerCsiHandler takes. */
interface XtermCsiSpec {
  prefix?: string;
  params?: number[];
  final: string;
}

/** The xterm 5 constructor options wireTerminal passes (views/terminal.ts). */
interface XtermOptions {
  fontFamily?: string;
  fontSize?: number;
  scrollback?: number;
  allowProposedApi?: boolean;
  theme?: Record<string, string>;
}

/** What an addon has to offer loadAddon - dispose is optional because the search and
 *  fit instances never get one called directly (the terminal owns their lifetime). */
interface XtermAddon {
  dispose?(): void;
}

/** The Terminal instance surface the panel drives (views/terminal.ts is the whole reader). */
interface XtermTerminal {
  options: { fontSize?: number };
  buffer: { active: XtermBuffer };
  unicode: { activeVersion: string };
  parser: { registerCsiHandler(spec: XtermCsiSpec, cb: () => boolean | Promise<boolean>): void };
  open(parent: HTMLElement): void;
  write(data: string | Uint8Array, cb?: () => void): void;
  focus(): void;
  paste(text: string): void;
  dispose(): void;
  scrollToLine(line: number): void;
  scrollToBottom(): void;
  getSelection(): string;
  clearSelection(): void;
  hasSelection(): boolean;
  loadAddon(addon: XtermAddon): void;
  attachCustomKeyEventHandler(handler: (ev: KeyboardEvent) => boolean): void;
  onData(cb: (text: string) => void): { dispose(): void };
  onTitleChange(cb: (title: string) => void): { dispose(): void };
  onBell(cb: () => void): { dispose(): void };
  onSelectionChange(cb: () => void): { dispose(): void };
  onLineFeed(cb: () => void): { dispose(): void };
}

/** The constructors the five xterm shims hand back (one per vendored package). */
type XtermTerminalCtor = new (options: XtermOptions) => XtermTerminal;
type XtermFitAddonCtor = new () => XtermFitAddon;
type XtermUnicode11AddonCtor = new () => XtermAddon;
type XtermWebLinksAddonCtor = new () => XtermAddon;

interface XtermFitAddon extends XtermAddon {
  fit(): void;
  /* Present in the served addon-fit.js (verified) though missing from the 0.10.0
     typings mirror; the resize/open paths read it and tolerate undefined. */
  proposeDimensions(): { cols: number; rows: number } | undefined;
}

interface XtermWebglAddon extends XtermAddon {
  onContextLoss(cb: () => void): void;
  clearTextureAtlas(): void;
}

interface XtermSearchAddon extends XtermAddon {
  onDidChangeResults(cb: (res: { resultIndex: number; resultCount: number } | undefined) => void): void;
  findNext(term: string, opts?: { decorations?: Record<string, unknown> }): boolean;
  findPrevious(term: string, opts?: { decorations?: Record<string, unknown> }): boolean;
  clearDecorations(): void;
}

/** cronstrue's namespace as the vendored UMD exposes it (jobs.ts describeCron). */
interface CronstrueLib {
  toString(expression: string, options?: { throwExceptionOnParseError?: boolean; [key: string]: unknown }): string;
}
