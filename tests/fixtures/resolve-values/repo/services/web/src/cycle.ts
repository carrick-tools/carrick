async function sendItems(url: string): Promise<Response> {
  return fetch(url);
}

export async function relay(
  send: (url: string) => Promise<Response>,
  hops: number,
): Promise<Response> {
  if (hops > 0) {
    return relay(send, hops - 1);
  }
  return send("/v1/relay");
}

export async function start(): Promise<Response> {
  return relay(sendItems, 2);
}
