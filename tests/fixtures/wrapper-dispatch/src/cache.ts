/** A five-minute in-memory cache. Its `get` takes the producer, not a URL. */
export class Cache {
  private value: Promise<unknown[]> | null = null;

  async get(produce: () => Promise<unknown[]>): Promise<unknown[]> {
    if (!this.value) {
      this.value = produce();
    }
    return this.value;
  }
}
