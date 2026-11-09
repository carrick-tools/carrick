type Send = (url: string) => Promise<Response>;

export function go(send: Send): Promise<Response> {
  return send("/v1/twice");
}

export function first(...rest: [Send]): Promise<Response> {
  return go(...rest);
}

export function second(...rest: [Send]): Promise<Response> {
  return go(...rest);
}
