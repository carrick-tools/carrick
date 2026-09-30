import { catalog } from "./lib/catalog.js";

export function searchCatalog(query: string) {
  return catalog.search(query);
}
