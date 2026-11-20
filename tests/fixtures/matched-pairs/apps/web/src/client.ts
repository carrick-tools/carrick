export interface Widget {
  id: string;
  total: number;
}

export async function getJson<T>(url: string): Promise<T> {
  return (await fetch(url)).json();
}

export async function listItems(): Promise<Widget[]> {
  const r = await fetch('/list');
  return (await r.json()).items;
}

export async function countItems(): Promise<number> {
  const items = await listItems();
  return items.length;
}

export async function readTotal(): Promise<number> {
  const w: { id: string; total: number } = await (await fetch('/w/1')).json();
  return w.total;
}

export async function readNumericId(): Promise<number> {
  const w: { id: number } = await (await fetch('/w/2')).json();
  return w.id;
}

export async function readMissing(): Promise<string> {
  const w = await getJson('/w/3');
  return w.name;
}

export async function readFromSql(): Promise<number> {
  const w: { id: string; total: number } = await (await fetch('/sql/1')).json();
  return w.total;
}

export function announce(): void {
  bus.publish('widget.created', { id: 'w1' });
}

declare const bus: { publish(topic: string, payload: unknown): void };
