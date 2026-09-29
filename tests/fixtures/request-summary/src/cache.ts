/**
 * The client's read-through cache. Mirrors carrick-cloud's mcp-server cache:
 * the producer is handed in, invoked with an argument of the cache's own, and
 * its promise is chained. `get` takes the producer, not a URL.
 */
export type Fetcher = (
  knownDigest: string | null,
) => Promise<{ data: unknown[]; digest: string | null }>;

export class Cache {
  private entry: { data: unknown[]; digest: string | null } | null = null;
  private inflight: Promise<unknown[]> | null = null;

  async get(fetcher: Fetcher): Promise<unknown[]> {
    if (this.entry) {
      return this.entry.data;
    }
    if (this.inflight) {
      return this.inflight;
    }
    const held = this.entry;
    this.inflight = fetcher(held?.digest ?? null)
      .then((outcome) => {
        this.entry = outcome;
        return outcome.data;
      })
      .finally(() => {
        this.inflight = null;
      });
    return this.inflight;
  }

  invalidate(): void {
    this.entry = null;
  }
}
