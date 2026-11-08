// Negative: the same call shape over a client that sends nothing. A verb
// member handed `{ url }` is not a request when nothing it runs sends one.
type Entry = { url: string; value?: unknown };
export type Cache = { get: (entry: Entry) => unknown; put: (entry: Entry) => void };

export const createCache = (): Cache => {
  const store = new Map<string, unknown>();
  return {
    get: (entry) => store.get(entry.url),
    put: (entry) => {
      store.set(entry.url, entry.value);
    },
  } as Cache;
};

export const cache = createCache();

export const readCached = (options?: { cache?: Cache }) =>
  (options?.cache ?? cache).get({ url: '/n1/cached' });

export const writeCached = (value: unknown) => cache.put({ url: '/n1/cached', value });
