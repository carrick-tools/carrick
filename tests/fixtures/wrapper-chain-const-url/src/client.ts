const config = {
  ordersApiUrl: process.env.ORDERS_API_URL ?? "http://localhost:4000",
};

function send(url: string, options: RequestInit, label: string): Promise<Response> {
  return tracer.span(label, async () => {
    const response = await fetch(url, { ...options, headers: traceHeaders() });
    return response;
  });
}

async function sendWithRetry(url: string, options: RequestInit, label: string) {
  const first = await send(url, options, label);
  if (first.status !== 401) {
    return first;
  }
  const retried = { ...options, headers: { Authorization: await refreshToken() } };
  return send(url, retried, label);
}

export async function callApi(method: string, endpoint: string, data?: object) {
  const url = `${config.ordersApiUrl}${endpoint}`;
  const init: RequestInit = {
    method,
    headers: { "Content-Type": "application/json" },
    body: data ? JSON.stringify(data) : undefined,
  };
  const response = await sendWithRetry(url, init, endpoint);
  return response.json();
}

export const ordersApi = {
  list: () => callApi("GET", "/v1/orders"),
  get: (orderId: string) => callApi("GET", `/v1/orders/${orderId}`),
  cancel: (orderId: string) => callApi("PATCH", `/v1/orders/${orderId}/cancel`, {}),
  download: async (orderId: string) => {
    const endpoint = `/v1/orders/${orderId}/invoice`;
    const url = `${config.ordersApiUrl}${endpoint}`;
    const options: RequestInit = { method: "GET" };
    const response = await sendWithRetry(url, options, endpoint);
    return response.blob();
  },
  search: (query: string) => callApi("GET", `/v1/orders/search${query ? `?${query}` : ""}`),
};
