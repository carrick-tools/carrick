import type { Steel } from "steel-sdk/edge";

const pool = new Map<string, unknown>();

export function checkoutClient(key: string): Steel {
  return pool.get(key) as Steel;
}
