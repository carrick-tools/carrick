// The caller. Nothing here states a path or a verb: each row belongs to a
// client member, and this file is what makes it a member the repo uses.
import type { ItemsClient } from "./client.js";

export async function isConnected(id: string, client: ItemsClient): Promise<boolean> {
  const item = await client.readItem(id);
  return item.connected;
}

export async function stateOf(id: string, client: ItemsClient): Promise<string> {
  const status = await client.readStatus(id);
  return status.state;
}

export async function connectedCount(client: ItemsClient): Promise<number> {
  const items = await client.listItems();
  return items.filter((item) => item.connected).length;
}
