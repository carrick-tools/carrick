export interface Order {
  id: string;
  total: number;
}

async function request<T>(path: string): Promise<T> {
  return (await fetch(path)).json();
}

export function fetchOrders(): Promise<Order[]> {
  return request<Order[]>('/orders');
}

export async function countOrders(): Promise<number> {
  const orders = await fetchOrders();
  return orders.length;
}

export async function loadAll(): Promise<unknown> {
  return Promise.all([fetchOrders(), request('/stock')]);
}

export async function loadGraph(): Promise<unknown> {
  return gql('{ orders { id } }');
}

declare function gql(query: string): Promise<unknown>;

export async function loadRaw(): Promise<unknown> {
  return request<any>('/raw');
}
