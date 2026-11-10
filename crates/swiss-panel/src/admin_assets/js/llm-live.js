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

/* LLM notices (SPEC §llm.panel). One coalesced re-read per page. These topics
   stay subscribed while the document is hidden; the coalescer holds the read. */

import { busOn } from "./bus/bus.js";
import { coalescedRead } from "./bus/live.js";
import { Module, NoticeSchema } from "./bus/pb/swiss/bus/v1/bus_pb.js";
import { HostSub, VaultId } from "./bus/pb/swiss/bus/v1/host_pb.js";
import { LlmConfigId, LlmHistoryId, LlmSub, TelemetryId } from "./bus/pb/swiss/bus/v1/llm_pb.js";

function follow(module        , sub        , id        , read                        )             {
  const burst = coalescedRead(read);
  const off = busOn({ module, sub, key: "" }, id, NoticeSchema, () => { burst.push(); });
  return () => {
    off();
    burst.stop();
  };
}

/** Re-read status on LLM.CONFIG. */
export function followLlmConfig(read                        )             {
  return follow(Module.LLM, LlmSub.CONFIG, LlmConfigId.LLM_CONFIG_CHANGED, read);
}

/** Re-read the history lists on LLM.HISTORY. */
export function followLlmHistory(read                        )             {
  return follow(Module.LLM, LlmSub.HISTORY, LlmHistoryId.LLM_HISTORY_CHANGED, read);
}

/** Agents: one re-read for CONFIG, TELEMETRY and HOST.VAULT. */
export function followAgentStatus(read                        )             {
  const burst = coalescedRead(read);
  const push = ()       => { burst.push(); };
  const offConfig = busOn(
    { module: Module.LLM, sub: LlmSub.CONFIG, key: "" },
    LlmConfigId.LLM_CONFIG_CHANGED,
    NoticeSchema,
    push,
  );
  const offTelemetry = busOn(
    { module: Module.LLM, sub: LlmSub.TELEMETRY, key: "" },
    TelemetryId.TELEMETRY_CHANGED,
    NoticeSchema,
    push,
  );
  const offVault = busOn(
    { module: Module.HOST, sub: HostSub.VAULT, key: "" },
    VaultId.VAULT_CHANGED,
    NoticeSchema,
    push,
  );
  return () => {
    offConfig();
    offTelemetry();
    offVault();
    burst.stop();
  };
}
