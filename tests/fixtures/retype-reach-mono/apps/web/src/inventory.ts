import type { CatalogClient } from "./client.js";

export async function countActive(
  widgetId: string,
  client: CatalogClient
): Promise<number> {
  const widget = await client.readWidget(widgetId);
  return widget.activeCount;
}
