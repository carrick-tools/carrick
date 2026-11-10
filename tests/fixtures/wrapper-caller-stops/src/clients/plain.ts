export function getItem(id: string): Promise<Response> {
  return fetch(`/items/${id}`);
}
