export interface OrderDraft {
  sku: string;
  quantity: number;
}

export function buildOrderLookup(baseUrl: string, orderId: string, token: string) {
  return {
    url: `${baseUrl}/api/v2/orders/${encodeURIComponent(orderId)}`,
    method: "GET",
    headers: { Authorization: `Bearer ${token}` },
  };
}

export function buildOrderCreate(baseUrl: string, token: string, draft: OrderDraft) {
  return {
    url: `${baseUrl}/api/v2/orders`,
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify(draft),
  };
}

export function buildStaleLookup(baseUrl: string, orderId: string) {
  return {
    url: `${baseUrl}/api/v1/orders/${orderId}`,
    method: "GET",
    headers: { Accept: "application/json" },
  };
}

export function noticeInit(location: string) {
  return { headers: { Location: location } };
}

export function routeTable(handler: () => void) {
  return [{ method: "GET", path: "/health", handler }];
}

export function buildWholePath(path: string) {
  return { url: path, method: "GET" };
}

export function buildGlued(baseUrl: string) {
  return { url: `${baseUrl}api/v2/orders`, method: "GET" };
}
