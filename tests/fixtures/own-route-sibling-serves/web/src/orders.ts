export async function loadOrder(id: string) {
  const response = await fetch(`/api/orders/${id}`);
  return response.json();
}

export async function loadStatus() {
  const response = await fetch("/api/status");
  return response.json();
}
