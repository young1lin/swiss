// Pure safety checks for the e2e overlay: nothing here reads the environment,
// touches the filesystem, starts, stops or deletes anything. The unit preflight
// (safety.test.mjs) imports this file alone; helpers.mjs enforces the verdicts
// at import time (before any boot) and again at every delete/kill site.
//
// Why this exists: the env knobs (SWISS_E2E_PORT/HOME/EXE) are powerful enough
// to aim the harness at production. 19999 is the human's instance and is never
// a test target; the production home, the live-verify snapshot home
// (swiss-test-home), the user profile, the filesystem root, the workspace and
// the deploy-source exe are equally off limits. Fail fast, with reasons, before
// anything runs.
import { isAbsolute, join, parse as parsePath, resolve, sep } from "node:path";

// The only sanctioned e2e ports: the suite default and the verification overlay.
export const SAFE_PORTS = ["18999", "19998"];

function digitsOnly(s) {
  return s.length > 0 && [...s].every((c) => c >= "0" && c <= "9");
}
function samePath(a, b) {
  return resolve(String(a)).toLowerCase() === resolve(String(b)).toLowerCase();
}
function pathUnder(path, root) {
  const p = resolve(String(path)).toLowerCase();
  const r = resolve(String(root)).toLowerCase();
  return p.startsWith(r.endsWith(sep) ? r : r + sep);
}

export function checkPort(port, allowedPorts) {
  const allowed = (allowedPorts || SAFE_PORTS).map(String);
  const reasons = [];
  const s = String(port == null ? "" : port).trim();
  if (!digitsOnly(s)) {
    reasons.push("port " + JSON.stringify(String(port)) + " is not a plain port number");
  } else {
    const n = Number(s);
    if (n < 1 || n > 65535) reasons.push("port " + s + " is out of range");
    else if (!allowed.includes(s)) {
      reasons.push("port " + s + " is not a sanctioned e2e port (allowed: " + allowed.join(", ") + ")");
      if (s === "19999") reasons.push("19999 is production and is never a test target");
    }
  }
  return { ok: reasons.length === 0, reasons };
}

// ctx: { prodHomes: string[], userProfile: string, workspaceRoot: string, protectedMarkers: string[] }
// A home is deletable only when it is a dedicated e2e home (a swiss-e2e path
// segment) and none of the protected trees. Legit custom e2e homes
// (.../swiss-e2e-anything) stay allowed; everything else is refused.
export function checkHome(home, ctx) {
  const reasons = [];
  if (home == null || String(home).trim() === "") {
    return { ok: false, reasons: ["the e2e home is empty"] };
  }
  const h = resolve(String(home));
  if (!isAbsolute(h)) reasons.push("the e2e home did not resolve to an absolute path: " + h);
  const root = parsePath(h).root;
  if (root && samePath(h, root)) reasons.push("the e2e home is the filesystem root: " + h);
  if (samePath(h, ctx.userProfile)) reasons.push("the e2e home is the user profile itself: " + h);
  if (samePath(h, ctx.workspaceRoot)) reasons.push("the e2e home is the workspace root itself: " + h);
  for (const prod of ctx.prodHomes || []) {
    if (!prod) continue;
    if (samePath(h, prod)) reasons.push("the e2e home is the production home: " + h);
    else if (pathUnder(h, prod)) reasons.push("the e2e home sits under the production home: " + h);
  }
  for (const marker of ctx.protectedMarkers || []) {
    if (String(marker) && h.toLowerCase().split(sep).includes(String(marker).toLowerCase())) {
      reasons.push("the e2e home touches the protected " + marker + " snapshot: " + h);
    }
  }
  if (!h.toLowerCase().split(sep).some((part) => part.includes("swiss-e2e"))) {
    reasons.push("the e2e home has no swiss-e2e path segment, so it does not look like a dedicated e2e home: " + h);
  }
  return { ok: reasons.length === 0, reasons };
}

// The exe the suite spawns may never be the deploy source or the running
// production binary; the overlay points at target-test.
export function checkExe(exe, ctx) {
  const reasons = [];
  if (exe == null || String(exe).trim() === "") {
    return { ok: false, reasons: ["the e2e exe is empty"] };
  }
  const e = resolve(String(exe));
  const banned = [
    [join(ctx.workspaceRoot, "target", "release", "swiss.exe"), "the production deploy source (target/release)"],
    [join(ctx.workspaceRoot, "bin", "swiss.exe"), "the running production binary (bin)"],
  ];
  for (const [path, why] of banned) {
    if (samePath(e, path)) reasons.push("the e2e exe is " + e + " - " + why + "; build and point at target-test instead");
  }
  return { ok: reasons.length === 0, reasons };
}

export function preflight(values, ctx) {
  const reasons = [];
  for (const r of checkPort(values.port).reasons) reasons.push(r);
  for (const r of checkHome(values.home, ctx).reasons) reasons.push(r);
  for (const r of checkExe(values.exe, ctx).reasons) reasons.push(r);
  return { ok: reasons.length === 0, reasons };
}
