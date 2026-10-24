import { useQuery } from '@fixture/query';
import { fetchPath, listItems, replaceItem, saveItem, Item } from './client';

export function useItems() {
  return useQuery({ queryFn: () => listItems() });
}

export async function rename(item: Item): Promise<Item> {
  return replaceItem(item.id, item);
}

export async function create(item: Item): Promise<Item> {
  return saveItem(item);
}

export async function loadAll(path: string): Promise<Item[]> {
  return fetchPath(path);
}
