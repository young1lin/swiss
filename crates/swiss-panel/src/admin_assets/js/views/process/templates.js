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
 * node on Unix (SPEC §process.apps image). `--port` pins the port the row declares, so a
 * changed dsh default can never split the two. */
import { tk } from "../../i18n.js";
                                           

                              
                                         
                
                                                                                           
              
                                                                                      
                
 

const DSH_PORT = 3080;

function dsh(...flags          )           {
  return {
    name: "dsh",
    start: { program: "dsh", args: ["web", ...flags, "--port", String(DSH_PORT)] },
    ports: [DSH_PORT],
    image: "node",
  };
}

/* Option order is the select's order. */
export const APP_TEMPLATES                              = {
  "dsh-web": { label: tk("process.template.dshWeb"), why: tk("process.template.dshWebWhy"), app: dsh() },
  "dsh-web-no-open": { label: tk("process.template.dshWebNoOpen"), why: tk("process.template.dshWebNoOpenWhy"), app: dsh("--no-open") },
};
