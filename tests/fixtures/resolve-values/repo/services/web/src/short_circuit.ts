async function sendOne(url: string): Promise<Response> {
  return fetch(url);
}

export function maybe(on: boolean): Promise<Response> {
  return (on && sendOne)("/v1/one");
}
