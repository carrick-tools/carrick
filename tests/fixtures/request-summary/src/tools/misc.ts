import { pathOf, readCached, registerRoutes } from "../negatives.js";

export function warm(router: { get(path: string, handler: () => void): void }) {
  registerRoutes(router);
  return [readCached(), pathOf("/x")];
}
