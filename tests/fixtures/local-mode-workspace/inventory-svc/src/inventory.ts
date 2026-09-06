// The caller. Nothing here states a path or a verb: the row belongs to the
// client member, and this file is what makes it a member the repo uses.
import type { CatalogClient } from "./client.js";

export async function countActive(
  widgetId: string,
  client: CatalogClient
): Promise<number> {
  const widget = await client.readWidget(widgetId);
  return widget.activeCount;
}

export async function nameOf(
  widgetId: string,
  client: CatalogClient
): Promise<string> {
  const widget = await client.readWidget(widgetId);
  return widget.name;
}
