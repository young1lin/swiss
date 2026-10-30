// Unit preflight for the pure safety checks. Imports safety.mjs ONLY: starts
// nothing, stops nothing, deletes nothing. Invalid ports are proven by verdict,
// never by calling stop() - a refusal test must not wield the tool it refuses.
import assert from "node:assert/strict";
import { SAFE_PORTS, checkExe, checkHome, checkPort, preflight } from "./safety.mjs";

const CTX = {
  prodHomes: ["C:/Users/tester/.swiss", "D:/prod-home"],
  userProfile: "C:/Users/tester",
  workspaceRoot: "C:/dev/swiss",
  protectedMarkers: ["swiss-test-home"],
};

// -- ports: the suite default and the verification overlay only ----------------
assert.deepEqual(SAFE_PORTS, ["18999", "19998"], "the sanctioned ports are exactly the two");
assert.equal(checkPort("18999").ok, true, "the suite default port is sanctioned");
assert.equal(checkPort("19998").ok, true, "the verification overlay port is sanctioned");
assert.equal(checkPort(19998).ok, true, "a numeric overlay port is accepted");
{
  const prod = checkPort("19999");
  assert.equal(prod.ok, false, "production is never a test target");
  assert.ok(prod.reasons.join(" | ").includes("19999 is production"), "the refusal names production: " + prod.reasons.join(" | "));
}
assert.equal(checkPort("30000").ok, false, "an arbitrary port is refused");
assert.equal(checkPort("0").ok, false, "port 0 is refused");
assert.equal(checkPort("70000").ok, false, "an out-of-range port is refused");
assert.equal(checkPort("abc").ok, false, "a non-numeric port is refused");
assert.equal(checkPort("").ok, false, "an empty port is refused");
assert.equal(checkPort(null).ok, false, "a null port is refused");

// -- homes: dedicated e2e homes only, customs preserved ------------------------
assert.equal(checkHome("C:/Users/tester/swiss-e2e", CTX).ok, true, "the default e2e home is allowed");
assert.equal(checkHome("C:/Users/tester/AppData/Local/swiss-e2e-19998", CTX).ok, true, "the dedicated overlay home is allowed");
assert.equal(checkHome("D:/tmp/swiss-e2e-custom", CTX).ok, true, "a legit custom e2e home is preserved");
assert.equal(checkHome("C:/Users/tester/.swiss", CTX).ok, false, "the production home is refused");
{
  const under = checkHome("C:/Users/tester/.swiss/child", CTX);
  assert.equal(under.ok, false, "a home under the production home is refused");
  assert.ok(under.reasons.join(" | ").includes("under the production home"), "the refusal says why: " + under.reasons.join(" | "));
}
assert.equal(checkHome("D:/prod-home", CTX).ok, false, "an env-pinned production home is refused too");
assert.equal(checkHome("C:/Users/tester", CTX).ok, false, "the user profile itself is refused");
assert.equal(checkHome("C:/dev/swiss", CTX).ok, false, "the workspace root is refused");
assert.equal(checkHome("C:/", CTX).ok, false, "the filesystem root is refused");
assert.equal(checkHome("C:/Users/tester/AppData/Local/swiss-test-home", CTX).ok, false, "the live-verify snapshot home is refused");
assert.equal(checkHome("C:/Users/tester/AppData/Local/swiss-test-home/sub", CTX).ok, false, "a directory under the snapshot home is refused");
assert.equal(checkHome("C:/Users/tester/ordinary", CTX).ok, false, "a home with no swiss-e2e segment is refused");
assert.equal(checkHome("", CTX).ok, false, "an empty home is refused");

// -- exes: never the deploy source or the running production binary -----------
assert.equal(checkExe("C:/dev/swiss/target/release/swiss.exe", CTX).ok, false, "the deploy-source exe is refused");
assert.equal(checkExe("C:/dev/swiss/bin/swiss.exe", CTX).ok, false, "the running production binary is refused");
assert.equal(checkExe("C:/dev/swiss/target-test/release/swiss.exe", CTX).ok, true, "the target-test exe is the sanctioned binary");
assert.equal(checkExe("D:/other/build/swiss.exe", CTX).ok, true, "an unrelated absolute exe is allowed");

// -- the combined preflight -----------------------------------------------------
{
  const bad = preflight({ port: "19999", home: "C:/Users/tester/.swiss", exe: "C:/dev/swiss/target/release/swiss.exe" }, CTX);
  assert.equal(bad.ok, false);
  assert.ok(bad.reasons.length >= 3, "every dangerous value is named: " + bad.reasons.join(" | "));
}
{
  const good = preflight({ port: "19998", home: "C:/Users/tester/AppData/Local/swiss-e2e-19998", exe: "C:/dev/swiss/target-test/release/swiss.exe" }, CTX);
  assert.equal(good.ok, true, "the sanctioned overlay passes clean: " + good.reasons.join(" | "));
}

console.log("safety: ok");
