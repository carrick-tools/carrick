import { itemsApi, Item } from './client';

export async function rename(item: Item): Promise<Item> {
  return itemsApi.replace(item.id, item);
}

export async function create(item: Item): Promise<Item> {
  return itemsApi.save(item);
}
