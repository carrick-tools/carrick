export interface Order { id: string }
export async function fetchOrders(path: string) {
  const response = await fetch(path);
  const body = await response.json();
  return { body, response };
}
