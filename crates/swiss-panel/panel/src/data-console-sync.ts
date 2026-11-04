/* Per-console serialization for browser edits. A failed compare-and-swap keeps the draft and
 * stops retries until the caller explicitly resolves it; it never rebases text silently. */
export interface ConsoleRecord {
  id: string;
  tab: string;
  agent: boolean;
  agentTab: string | null;
  open: boolean;
  sql: string;
  revision: number;
  createdAt: number;
  updatedAt: number;
  closedAt: number | null;
}

type Write = (conn: string, record: ConsoleRecord) => Promise<ConsoleRecord>;
interface Item { value: ConsoleRecord; generation: number; saved: number; failed: boolean; reason?: string; active?: Promise<boolean> }

/** What the page should say about one record. `failed` is a conflict or a refused write: the
 *  local text is still the reader's and nothing more is sent until it is resolved. */
export type SyncState = "clean" | "pending" | "failed";

export class ConsoleSync {
  private readonly items = new Map<string, Item>();
  private readonly write: Write;
  constructor(write: Write) { this.write = write; }
  private key(conn: string, id: string): string { return JSON.stringify([conn, id]); }

  seed(conn: string, value: ConsoleRecord): boolean {
    const key = this.key(conn, value.id), item = this.items.get(key);
    if (item && (item.active || item.generation !== item.saved || value.revision < item.value.revision)) return false;
    this.items.set(key, { value: { ...value }, generation: 0, saved: 0, failed: false });
    return true;
  }

  stage(conn: string, value: ConsoleRecord): void {
    const key = this.key(conn, value.id), item = this.items.get(key);
    if (!item) { this.items.set(key, { value: { ...value }, generation: 1, saved: 0, failed: false }); return; }
    // A staged edit is not a reason to retry a refused save: the conflict stays until resolved.
    // The revision only moves forward: a stale tab object must never talk the queue back down
    // to a record the server has already revised past.
    item.value = { ...value, revision: Math.max(item.value.revision, value.revision) };
    item.generation++;
  }

  pending(conn: string): ConsoleRecord[] {
    return [...this.items.entries()].filter(([key, item]) => JSON.parse(key)[0] === conn && item.generation !== item.saved)
      .map(([, item]) => ({ ...item.value }));
  }

  value(conn: string, id: string): ConsoleRecord | null {
    const item = this.items.get(this.key(conn, id));
    return item ? { ...item.value } : null;
  }

  state(conn: string, id: string): SyncState {
    const item = this.items.get(this.key(conn, id));
    if (!item || (item.generation === item.saved && !item.failed)) return "clean";
    return item.failed ? "failed" : "pending";
  }

  /** Why a failed record was refused, in the words the write threw (already the reader's
   *  language: the store maps the server's code). Null while nothing failed. */
  reason(conn: string, id: string): string | null {
    const item = this.items.get(this.key(conn, id));
    return item && item.failed ? item.reason || "" : null;
  }

  /** Adopt the server's revision after a conflict the reader chose to overwrite: the draft stays
   *  exactly as typed and the next flush sends it against the record now on the server. */
  rebase(conn: string, id: string, server: ConsoleRecord): void {
    const key = this.key(conn, id), item = this.items.get(key);
    if (!item) { this.seed(conn, { ...server }); return; }
    item.value.revision = server.revision;
    item.value.createdAt = server.createdAt;
    item.value.closedAt = server.closedAt;
    item.failed = false;
    item.reason = undefined;
    item.generation++;
  }

  /** Adopt the server's text after the reader chose it over the local draft. */
  adopt(conn: string, server: ConsoleRecord): void {
    this.items.set(this.key(conn, server.id), { value: { ...server }, generation: 0, saved: 0, failed: false });
  }

  forget(conn: string, id: string): void {
    this.items.delete(this.key(conn, id));
  }

  private async save(conn: string, item: Item): Promise<boolean> {
    while (item.saved !== item.generation) {
      if (item.failed) return false;
      const generation = item.generation, value = { ...item.value };
      try {
        const saved = await this.write(conn, value);
        item.value.revision = saved.revision;
        item.value.createdAt = saved.createdAt;
        item.value.updatedAt = saved.updatedAt;
        item.value.closedAt = saved.closedAt;
        item.saved = generation;
      } catch (e) {
        item.failed = true;
        item.reason = e instanceof Error ? e.message : String(e);
        return false;
      }
    }
    return true;
  }

  async flush(conn: string): Promise<boolean> {
    const all = [...this.items.entries()].filter(([key]) => JSON.parse(key)[0] === conn).map(([, item]) => item);
    const results = await Promise.all(all.map(async item => {
      if (!item.active) item.active = this.save(conn, item);
      const pending = item.active;
      const ok = await pending;
      if (item.active === pending) item.active = undefined;
      return ok;
    }));
    return results.every(Boolean);
  }
}
