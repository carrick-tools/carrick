export async function call<T>(path: string, init?: RequestInit): Promise<{ status: number; body: T }> {
  const res = await fetch(`/api${path}`, { ...init, headers: { accept: "application/json" } });
  const body: T = await res.json();
  return { status: res.status, body };
}
export async function loadOrder() {
  const { body } = await call<{ id: number; slug: string }>("/orders/1");
  return body;
}
export async function loadCount() {
  return (await call<{ count: number }>("/counts")).body;
}
