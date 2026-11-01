import { getItem, postTo } from "@fixture/client";

export async function show(id: string) {
  return getItem(id);
}

export async function track() {
  return postTo("/api/events", { at: 1 });
}
