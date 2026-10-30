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

/* Process API contracts shared by the page and its sheets. */
export interface LastAction { action: string; ok: boolean; message: string; at: number; canForce?: boolean }

/* The row of GET /api/process (SPEC §process.apps), camelCase on the wire. */
export interface ProcessRow {
  name: string;
  status: "running" | "partial" | "conflict" | "stopped" | "unknown";
  reason?: string;
  pid?: number;
  ports: number[];
  downPorts?: number[];
  heldPorts?: number[];
  conflictPid?: number;
  conflictImage?: string;
  uptimeSec?: number;
  busy?: string;
  lastAction?: LastAction;
  autostart: boolean;
  ui?: string;
  stopIsKill: boolean;
  hasAppLog: boolean;
}

export interface GatewayFacts { pid: number; uptimeSec: number; memMb: number }

/* One child of the gateway (SPEC §process.children): stop goes through the
 * owner's hook, never a raw kill - a row without one shows no Stop. */
export interface ChildRow { pid: number; owner: string; kind: string; label: string; uptimeSec: number; memMb?: number; stoppable: boolean }

export interface HistoryEntry { ts: number; app: string; action: string; by: string; ok: boolean; message: string; pid?: number; exitCode?: number; forced?: boolean }

export interface LogAnswer { source: string; path: string; size: number; lines: string[]; truncated: boolean; reset: boolean }

/* The app entry as the config store holds it (SPEC §process.apps): argv, never a
 * string to split; env values stay references exactly as typed. */
export interface AppEntry {
  name: string;
  start: { program: string; args?: string[] };
  stop?: { program: string; args?: string[] };
  dir?: string;
  ports?: number[];
  image?: string;
  log?: string;
  ui?: string;
  autostart?: boolean;
  env?: Record<string, string>;
}
/* Wire shapes only: the page, the form and the logs share this one typed view of the API. */
