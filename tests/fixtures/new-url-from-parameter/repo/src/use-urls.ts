import { removeItem, removeItemByUrl, removeItemThroughBuilder } from "./urls";

export async function dropItems() {
  await removeItem("/items.remove");
  await removeItemByUrl("/items.remove");
  await removeItemThroughBuilder("/items.drop");
}

// An absolute URL resolves to itself: the base is not part of it.
export async function dropRemote() {
  await removeItem("https://remote.example.com/remote.remove");
}

// A relative path with no leading slash resolves against the base's own
// path, so it names no route.
export async function dropRelative() {
  await removeItem("items");
}
