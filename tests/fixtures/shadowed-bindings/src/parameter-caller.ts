import { archiveItem, removeItem } from "./parameter";

export async function clearItems() {
  await removeItem("/api/param-users", true);
  return archiveItem("/api/param-archive");
}
