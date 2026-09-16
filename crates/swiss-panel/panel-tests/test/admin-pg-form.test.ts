import { beforeAll, describe, expect, it } from "vitest";

/* docs/30 — the pg url <-> fields pair. Pure string work, so no DOM: what is pinned is the
   round-trip (incl. ${...} refs and the mask sentinel) and the submit-path translation. */
let fields: typeof import("../../src/admin_assets/js/fields.js");

beforeAll(async () => {
  fields = await import("../../src/admin_assets/js/fields.js");
});

describe("docs/30: parsePgUrl / pgUrlFrom", () => {
  it("a full url splits, and the pieces rebuild the same url", () => {
    const url = "postgresql://shop:hunter2@127.0.0.1:5432/shop?sslmode=disable";
    expect(fields.parsePgUrl(url)).toEqual({
      host: "127.0.0.1", port: "5432", user: "shop",
      password: "hunter2", database: "shop", params: "sslmode=disable",
    });
    expect(fields.pgUrlFrom(fields.parsePgUrl(url)!)).toBe(url);
  });

  it("a password ref survives the split — the envelope claims its ':' and '/'", () => {
    const url = "postgresql://mm:${secret://pg-main}@db.local:5432/mm";
    const parts = fields.parsePgUrl(url)!;
    expect(parts.password).toBe("${secret://pg-main}");
    expect(parts.host).toBe("db.local");
    expect(fields.pgUrlFrom(parts)).toBe(url);
  });

  it("the mask sentinel rides verbatim — only the server may resolve it", () => {
    const url = "postgresql://mm:••••••••@db.local:5432/mm";
    expect(fields.parsePgUrl(url)!.password).toBe("••••••••");
    expect(fields.pgUrlFrom(fields.parsePgUrl(url)!)).toBe(url);
  });

  it("optional pieces stay optional: no credentials, no port, multi-line params", () => {
    const url = "postgresql://db.local/mydb?sslmode=disable&application_name=swiss";
    const parts = fields.parsePgUrl(url)!;
    expect(parts.user).toBe("");
    expect(parts.port).toBe("");
    expect(parts.params).toBe("sslmode=disable\napplication_name=swiss");
    expect(fields.pgUrlFrom(parts)).toBe(url);
  });

  it("garbage returns null — the caller falls back to the raw field", () => {
    expect(fields.parsePgUrl("not a url at all")).toBeNull();
    expect(fields.parsePgUrl("")).toBeNull();
  });
});

describe("docs/30: translatePg — the submit path", () => {
  it("assembles the url and drops the part keys", () => {
    const body = fields.translatePg("pg", {
      type: "pg", host: "h", port: 5432, user: "u", password: "p", database: "d",
      params: "sslmode=disable", description: "x", maxRows: 200,
    });
    expect(body.url).toBe("postgresql://u:p@h:5432/d?sslmode=disable");
    expect(body.host).toBeUndefined();
    expect(body.password).toBeUndefined();
    expect(body.description).toBe("x");
  });

  it("other types pass through untouched — mysql owns the same field names", () => {
    const body = { type: "mysql", host: "h", user: "u", password: "p" };
    expect(fields.translatePg("mysql", body)).toBe(body);
    expect(body).toEqual({ type: "mysql", host: "h", user: "u", password: "p" });
  });

  it("an unparseable url rides through as __pgRaw when no part is filled", () => {
    const body = fields.translatePg("pg", { type: "pg", __pgRaw: "postgres://weird" });
    expect(body.url).toBe("postgres://weird");
    expect(body.__pgRaw).toBeUndefined();
  });

  it("a whole-value ref stays a ref untouched — the common stored shape", () => {
    const body = fields.translatePg("pg", { type: "pg", __pgRaw: "${SHOP_DB_URL}" });
    expect(body.url).toBe("${SHOP_DB_URL}");
  });

  it("typing into any part field decomposes: parts win over the raw field", () => {
    const body = fields.translatePg("pg", {
      type: "pg", __pgRaw: "${SHOP_DB_URL}",
      host: "db.local", user: "mm", password: "${secret://pg-main}", database: "mm",
    });
    expect(body.url).toBe("postgresql://mm:${secret://pg-main}@db.local/mm");
    expect(body.__pgRaw).toBeUndefined();
  });
});
