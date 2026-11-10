import { ItemStore } from "./clients/store";

const items = new ItemStore();

export async function loadItems(): Promise<Response> {
  return items.list();
}
