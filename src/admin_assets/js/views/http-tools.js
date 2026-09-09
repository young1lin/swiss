/* ================================================================================================
   HTTP Tools - the touchstone plugin's page (docs/09 §9, docs/12 W4).

   A minimal request builder + response viewer. The form is generated ENTIRELY from the
   action's own schema on GET /api/actions (run.js's builder, the same one the MCP Run
   view and the jobs editor use) - this file never spells the fields itself, so the tool
   and its form cannot drift apart. Runs go through the shared execution surface
   (POST /api/runs, GET /api/runs/{id}); when the http-tools plugin is disabled the
   action is simply absent and the page says so.
   ================================================================================================ */
import { $, apiJson, esc } from "../util.js";
import { argFieldsHtml, readRunArgs } from "../run.js";

var action = null;   // the http.request row from /api/actions (null = not provided)
var run = null;      // the latest run view, while it is in flight or after it settled
var timer = null;    // the poll loop's interval handle

function metaLine() {
  if (!run) return "";
  var bits = ["run " + run.runId, run.state];
  if (run.ms != null) bits.push(run.ms + " ms");
  if (run.chars != null && run.outputTruncated) bits.push("output capped, " + run.chars + " chars total");
  if (run.error) bits.push(run.error);
  return esc(bits.join(" · "));
}

function outBlock() {
  if (!run) return '<pre class="logs" id="ht-out" hidden></pre>';
  var text = run.output == null ? "" : run.output;
  return '<pre class="logs" id="ht-out">' + esc(text) + "</pre>";
}

function render() {
  var pane = $("pane");
  if (!pane) return;
  if (!action) {
    pane.innerHTML = '<div class="wide">' +
      '<div class="pane-head"><div><h1 class="pane-title">HTTP</h1>' +
        '<div class="pane-desc">Send one HTTP request from the gateway.</div></div></div>' +
      '<div class="empty"><div><h2>No http.request capability</h2>' +
        '<p class="hint">The http-tools plugin is disabled or stopped. Enable it on the Plugins page and this view comes back.</p></div></div>' +
      "</div>";
    return;
  }
  pane.innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">HTTP</h1>' +
      '<div class="pane-desc">One request through the shared run surface - cancellable, capped and masked exactly like any other run.</div></div></div>' +
    '<div class="group"><div class="form">' +
      argFieldsHtml({ inputSchema: action.schema }, "ht-", {}) +
      '<div class="form-actions"><button class="btn primary" id="ht-run">Run</button>' +
        '<span class="run-meta" id="ht-meta">' + metaLine() + "</span></div>" +
      outBlock() +
    "</div></div></div>";
  var button = $("ht-run");
  if (button) button.onclick = function () { void doRun(); };
}

async function load() {
  var reply = await apiJson("/api/actions");
  if (!reply) return; // the toast is enough; keep whatever we had
  action = (reply.actions || []).find(function (a) { return a.type === "http.request"; }) || null;
}

async function doRun() {
  if (!action || timer) return;
  var input = {};
  try {
    input = readRunArgs({ inputSchema: action.schema }, "ht-");
  } catch (error) {
    var meta = $("ht-meta");
    if (meta) meta.textContent = String(error.message || error);
    return;
  }
  if (!input.url) { $("ht-meta").textContent = "url is required"; return; }
  var reply = await apiJson("/api/runs", {
    method: "POST",
    body: JSON.stringify({ action: "http.request", input: input }),
  });
  if (!reply) return;
  run = reply.run || { runId: reply.runId, state: "queued" };
  render();
  timer = setInterval(function () { void pollRun(); }, 400);
}

/** The 400ms loop this page owns while a run is in flight; the exported poll() is the
 *  shell's slower cadence, which just steps the same loop once. */
async function pollRun() {
  if (run && run.runId != null && (run.state === "queued" || run.state === "running")) {
    var view = await apiJson("/api/runs/" + encodeURIComponent(String(run.runId)));
    if (view) { run = view; patch(); }
  }
  if (run && run.state !== "queued" && run.state !== "running") stopTimer();
}

function stopTimer() {
  if (timer) { clearInterval(timer); timer = null; }
}

/** Poll-safe update of the live bits - meta line and output - without rebuilding the
 *  form under the user's cursor. */
function patch() {
  var meta = $("ht-meta");
  if (meta) meta.innerHTML = metaLine();
  var out = $("ht-out");
  if (out) {
    out.hidden = false;
    out.textContent = run && run.output != null ? run.output : "";
  }
}

export async function mount() {
  action = null; run = null; stopTimer();
  await load();
  render();
}
export async function refresh() { await mount(); }
export async function poll() { await pollRun(); }
export function countText() {
  return run ? run.state : "";
}
export function unmount() { stopTimer(); action = null; run = null; }
