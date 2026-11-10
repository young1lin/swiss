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

/* MCP.SERVERS, held only while the document is visible (SPEC §mcp.registry). Hide
   sends UNSUBSCRIBE, which is what lets the health probe park. */

import { busOnWhileVisible, coalescedRead } from "./bus/live.js";
import { Module, NoticeSchema } from "./bus/pb/swiss/bus/v1/bus_pb.js";
import { McpSub, ServersId } from "./bus/pb/swiss/bus/v1/mcp_pb.js";

/** Re-read `GET /api/mcps` on MCP.SERVERS while this page is visible. */
export function followMcpServers(read: () => Promise<unknown>): () => void {
  const burst = coalescedRead(read);
  const off = busOnWhileVisible(
    { module: Module.MCP, sub: McpSub.SERVERS, key: "" },
    ServersId.SERVERS_CHANGED,
    NoticeSchema,
    () => { burst.push(); },
    () => { burst.push(); },
  );
  return () => {
    off();
    burst.stop();
  };
}
