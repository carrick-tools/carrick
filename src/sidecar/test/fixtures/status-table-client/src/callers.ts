import { listItems } from './sdk.gen';

export async function loadItems() {
  const result = await listItems();
  return result;
}

export async function firstItem() {
  const result = await listItems();
  if (result.error) {
    return null;
  }
  return result.data[0];
}
