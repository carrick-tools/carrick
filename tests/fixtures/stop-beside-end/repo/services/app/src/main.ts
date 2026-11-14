import { getItem, postTo, ItemsClient, format } from "@fixture/client";
import { ping } from "@fixture/loose";

export async function show(id: string) {
  return getItem(id);
}

export async function track() {
  return postTo("/api/events", { at: 1 });
}

export async function drop(id: string) {
  const items = new ItemsClient();
  return items.remove(id);
}

export function label(n: number): string {
  return format(n);
}

export function loose() {
  return ping();
}
