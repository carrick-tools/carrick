export type Item = { id: string; name: string; archived: boolean };

export async function listItems(): Promise<Item[]> {
  const res = await fetch('/api/items');
  return (await res.json()).items;
}

export async function getItem(id: string): Promise<Item> {
  const res = await fetch(`/api/items/${id}`);
  return res.json();
}

export async function getReport(from: string, to: string) {
  const res = await fetch(`/api/reports?from=${from}&to=${to}`);
  return res.json();
}

export async function setArchived(id: string, archived: boolean) {
  const method = archived ? 'POST' : 'DELETE';
  await fetch(`/api/items/${id}/archive`, { method });
}

export async function saveItem(item: Partial<Item>) {
  const url = item.id ? `/api/items/${item.id}` : '/api/items';
  const method = item.id ? 'PUT' : 'POST';
  const res = await fetch(url, { method, body: JSON.stringify(item) });
  return res.json();
}
