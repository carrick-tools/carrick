export async function widgets(): Promise<Response> {
  return fetch(`/v1/widgets`);
}

export async function widget(id: string): Promise<Response> {
  return fetch(`/v1/widgets/${id}`);
}

export async function ping(): Promise<Response> {
  return fetch(`/widgets/ping`);
}

export async function orders(): Promise<Response> {
  return fetch(`/v2/orders`);
}

export async function placeOrder(): Promise<Response> {
  return fetch(`/v2/orders`, { method: "POST" });
}

export async function daily(): Promise<Response> {
  return fetch(`/v3/reports/daily`);
}

export async function weekly(): Promise<Response> {
  return fetch(`/v1/reports/weekly`);
}

export async function health(): Promise<Response> {
  return fetch(`/status/health`);
}

// Decoys: the version the route is NOT served under, and no version at all.
export async function ordersUnderV1(): Promise<Response> {
  return fetch(`/v1/orders`);
}

export async function widgetsUnversioned(): Promise<Response> {
  return fetch(`/widgets`);
}
