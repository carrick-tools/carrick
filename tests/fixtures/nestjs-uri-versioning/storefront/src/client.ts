const CATALOG_BASE = process.env.CATALOG_URL ?? "";

export async function widgets(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v1/widgets`);
}

export async function widget(id: string): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v1/widgets/${id}`);
}

export async function ping(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/widgets/ping`);
}

export async function orders(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v2/orders`);
}

export async function placeOrder(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v2/orders`, { method: "POST" });
}

export async function daily(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v3/reports/daily`);
}

export async function weekly(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v1/reports/weekly`);
}

export async function health(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/status/health`);
}

// Decoys: the version the route is NOT served under, and no version at all.
export async function ordersUnderV1(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/v1/orders`);
}

export async function widgetsUnversioned(): Promise<Response> {
  return fetch(`${CATALOG_BASE}/widgets`);
}
