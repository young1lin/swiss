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

/* The link draws nothing while it is up. Down for more than a few seconds, one quiet
   line in the chrome says so, and WELCOME takes it away (SPEC §bus.link). The line is
   the library's note, so the page does not invent a chrome class. */

import { tr } from "../i18n.js";
import { note } from "../ui/page.js";

let line                     = null;

export function mountBusLine(down         )       {
  if (typeof document === "undefined") return;
  const host = document.querySelector(".workbench");
  if (!host) return;
  if (!down) {
    if (line) line.remove();
    line = null;
    return;
  }
  if (line && line.isConnected) return;
  line = drawn();
  host.insertBefore(line, host.firstChild);
}

export function repaintBusLine()       {
  if (!line || !line.isConnected) return;
  const next = drawn();
  line.replaceWith(next);
  line = next;
}

function drawn()              {
  const node = note(tr("bus.reconnecting"), { busy: true });
  node.setAttribute("role", "status");
  return node;
}
