import { getItem } from "@fixture/mid";

export async function show(id: string) {
  return getItem(id);
}
