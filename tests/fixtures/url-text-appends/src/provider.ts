const handlers = new Map<string, unknown>();

export function register(kind: string, impl: unknown): void {
  handlers.set(kind, impl);
}

export function boot(): void {
  register("catalog", {
    async lookup(opts: { term?: string }): Promise<string[]> {
      let target = "/api/catalog/entries";
      if (opts.term) {
        target += `?term=${encodeURIComponent(opts.term)}`;
      }
      const found: { entries?: string[] } = await fetch(target.toString()).then((r: Response) => r.json());
      return found.entries ?? [];
    },
  });
}
