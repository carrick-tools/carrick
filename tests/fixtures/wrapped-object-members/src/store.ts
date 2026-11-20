export interface Item {
  id: string;
  name: string;
}

const items: Item[] = [];

export async function listWidgets(): Promise<Item[]> {
  return items;
}

export async function findWidget(id: string): Promise<Item | undefined> {
  return items.find((item) => item.id === id);
}

export async function saveWidget(item: Item): Promise<Item> {
  items.push(item);
  return item;
}

export async function listGadgets(): Promise<Item[]> {
  return items;
}

export function findGadget(id: string): Item | undefined {
  return items.find((item) => item.id === id);
}

export async function removeGadget(id: string): Promise<void> {
  items.splice(items.findIndex((item) => item.id === id), 1);
}
