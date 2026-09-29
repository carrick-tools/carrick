// Calls shaped like requests that send nothing, and a route registration.
// None of them is a consumer row.
const store = new Map<string, unknown>();

export function readCached(): unknown {
  return store.get("/users");
}

export function registerRoutes(router: {
  get(path: string, handler: () => void): void;
}): void {
  router.get("/health", () => {});
}

export function pathOf(url: string): string {
  return new URL(url, "http://localhost").pathname;
}
