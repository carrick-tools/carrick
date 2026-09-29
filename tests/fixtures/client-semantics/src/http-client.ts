import http from "@fixture/http";

const api = http.create({ baseURL: "/api/v1", timeout: 5000 });

export async function listUsers(): Promise<unknown> {
  const response = await api.get("/users");
  return response.data;
}

export async function createOrder(sku: string): Promise<unknown> {
  const response = await api.post("/orders", { action: "create", sku });
  return response.data;
}

export async function syncInventory(): Promise<unknown> {
  return http.request({ url: "/inventory/sync", method: "post", data: { mode: "full" } });
}
