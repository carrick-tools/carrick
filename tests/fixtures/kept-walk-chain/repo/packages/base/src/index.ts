export async function send<T>(url: string, init: { method: string }): Promise<T> {
  const res = await fetch(url, init);
  return (await res.json()) as T;
}
