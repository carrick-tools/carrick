import type { Forge } from "forge-sdk/edge";

const pool = new Map<string, unknown>();

export function checkoutClient(key: string): Forge {
  return pool.get(key) as Forge;
}
