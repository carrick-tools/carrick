import { syncCatalog } from "../lib/catalog";

export const pushCatalog = (items: unknown[]) => syncCatalog(items);
