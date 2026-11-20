export async function apiGet<T>(path: string): Promise<T> {
  return (await fetch(path)).json();
}

export async function loadA(): Promise<unknown> {
  return apiGet('/a');
}

export async function loadB(): Promise<unknown> {
  return apiGet('/b');
}

export const loadC = (): Promise<unknown> => apiGet('/c');

export async function chained(): Promise<number> {
  const w: { id: string; total: number } = await fetch('/w/1').then((r) => r.json());
  return w.total;
}

export async function both(): Promise<unknown> {
  return Promise.all([fetch('/w/1'), fetch('/list')]);
}

export const options = {
  retries: 2,
};

export async function callFunction(): Promise<number> {
  const w: { id: string; total: number } = await (await fetch('/functions/v1/widget')).json();
  return w.total;
}

export async function readStock(): Promise<number> {
  const rows: { sku: string; qty: number }[] = await db.from('stock').select();
  return rows.length;
}

export async function widgetById(): Promise<number> {
  const rows: { id: string; total: number }[] = await db.rpc('widget_by_id');
  return rows.length;
}

declare const db: {
  from(table: string): { select(): Promise<{ sku: string; qty: number }[]> };
  rpc(name: string): Promise<{ id: string; total: number }[]>;
};
