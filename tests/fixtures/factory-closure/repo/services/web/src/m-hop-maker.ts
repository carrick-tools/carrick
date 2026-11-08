interface Req {
  <T>(method: string, path: string, body?: unknown): Promise<T>;
}
function make(o: { token: string; send?: typeof fetch }): Req {
  const doFetch = o.send ?? fetch;
  async function request(method: string, path: string, body?: unknown): Promise<unknown> {
    const response = await doFetch(`https://h.test${path}`, {
      method,
      headers: { Authorization: `Bearer ${o.token}` },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return response.text();
  }
  return request as Req;
}
function outer() {
  const hs = make({ token: 't' });
  async function run() { return hs<string>('POST', '/p/m-hop-maker', {}); }
  return run;
}
export function useOuter() { return outer()(); }
