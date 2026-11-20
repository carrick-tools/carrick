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
  const w: { id: string; total: number } = await (await fetch('/fn/resize')).json();
  return w.total;
}

export async function readStock(): Promise<number> {
  const rows: { sku: string; qty: number }[] = await store.rows('stock').read();
  return rows.length;
}

export async function widgetById(): Promise<number> {
  const rows: { id: string; total: number }[] = await store.call('widget_by_id');
  return rows.length;
}

declare const store: {
  rows(table: string): { read(): Promise<{ sku: string; qty: number }[]> };
  call(name: string): Promise<{ id: string; total: number }[]>;
};
