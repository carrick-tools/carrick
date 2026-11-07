type Send = (url: string) => Promise<Response>;

async function sendGo(url: string): Promise<Response> {
  return fetch(url);
}

export function go(send: Send): Promise<Response> {
  return send("/v1/go");
}

export function forward(...rest: [Send]): Promise<Response> {
  return go(...rest);
}

export function direct(): Promise<Response> {
  return go(sendGo);
}

export function lonely(send: Send = sendGo): Promise<Response> {
  return send("/v1/lonely");
}
