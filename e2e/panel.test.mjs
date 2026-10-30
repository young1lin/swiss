import { get, withGateway } from "./helpers.mjs";
import assert from "node:assert/strict";

await withGateway(async () => {
  // The panel shell itself sits behind the admin session (SPEC §host.session):
  // with no session cookie it must fail closed — this is the security property.
  {
    const r = await get("/");
    assert.ok(r.status === 401 || r.status === 403, "shell must fail closed without a session, got " + r.status);
  }
  // Static admin assets serve publicly and are never used unrevalidated: a stale panel is a
  // stale bug fix (the /admin routes send Cache-Control: no-cache with an ETag).
  for (const asset of ["/admin/js/main.js", "/admin/styles/ui.css"]) {
    const r = await get(asset);
    assert.equal(r.status, 200, asset + " serves");
    assert.ok(r.text.length > 100, asset + " has content");
    const cc = (r.headers.get("cache-control") || "").toLowerCase();
    assert.ok(cc.includes("no-store") || cc.includes("no-cache"), asset + " cache-control: " + cc);
  }
  {
    const r = await get("/admin/does-not-exist.js");
    assert.equal(r.status, 404, "unknown asset 404s");
  }
  console.log("panel: ok");
});
