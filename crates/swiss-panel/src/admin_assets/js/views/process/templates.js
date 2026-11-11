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
 * A template with `portInArgs` is a launch ON a port, and the port is the reader's: 3080 is
 * only where it starts, since another process may hold it. `--port` in its argv is the port the
 * row declares, so the form rebuilds the argv as Ports changes and the two can never split. An
 * emptied Ports is a launch on no declared port: no `--port` (the program picks its own) and no
 * image, since an image only names who may hold a declared port.
 *
 * A template without it carries its program's OWN ports (cc-otel's live in cc-otel.yaml): a pick
 * fills them, and Ports decides only whether there is an image. Either kind may also fill the
 * stop command and the UI, which the form treats like the name - see applyTemplate. */
import { tk } from "../../i18n.js";
                                           

                              
                                         
                
                                                                                           
              
                                                                                        
                  
                                                                                        
                      
                                                                                               
                                                                                            
                                                 
                                 
 

function dsh(...flags          )                                {
  return (ports) => ports.length === 0
    ? { name: "dsh", start: { program: "dsh", args: ["web", ...flags] } }
    : {
      name: "dsh",
      start: { program: "dsh", args: ["web", ...flags, "--port", String(ports[0])] },
      ports: [ports[0]],
      image: "node",
    };
}

/* cc-otel is a Go binary that `cc-otel install` puts in ~/.cc-otel, off PATH: the program is
 * home-relative, which the backend expands like `dir` (SPEC §process.apps), and resolution adds
 * .exe on Windows. `start` is a launcher - it forks the daemon and exits 0 - so the image is the
 * daemon's own (`cc-otel` matches cc-otel.exe on Windows), and Stop runs `cc-otel stop` rather
 * than a hard kill. The ports are cc-otel.yaml's defaults: 4317 OTLP gRPC, 4318 OTLP HTTP and
 * 8899 the dashboard, which is the UI. */
const CC_OTEL = "~/.cc-otel/cc-otel";
const CC_OTEL_PORTS = [4317, 4318, 8899];

function ccOtel(ports          )           {
  const app           = {
    name: "cc-otel",
    start: { program: CC_OTEL, args: ["start"] },
    stop: { program: CC_OTEL, args: ["stop"] },
    ui: "http://localhost:8899",
  };
  if (ports.length) {
    app.ports = ports.slice();
    app.image = "cc-otel";
  }
  return app;
}

/* Option order is the select's order. */
export const APP_TEMPLATES                              = {
  "dsh-web": { label: tk("process.template.dshWeb"), why: tk("process.template.dshWebWhy"), ports: [3080], portInArgs: true, app: dsh() },
  "dsh-web-no-open": { label: tk("process.template.dshWebNoOpen"), why: tk("process.template.dshWebNoOpenWhy"), ports: [3080], portInArgs: true, app: dsh("--no-open") },
  "cc-otel": { label: tk("process.template.ccOtel"), why: tk("process.template.ccOtelWhy"), ports: CC_OTEL_PORTS, portInArgs: false, app: ccOtel },
};

/* The one port a Ports text names, or null for none, several or a bad one. */
export function singlePort(text        )                {
  const parts = text.split(",").map((s) => s.trim()).filter(Boolean);
  if (parts.length !== 1 || !/^\d+$/.test(parts[0])) return null;
  const n = Number(parts[0]);
  return n >= 1 && n <= 65535 ? n : null;
}
