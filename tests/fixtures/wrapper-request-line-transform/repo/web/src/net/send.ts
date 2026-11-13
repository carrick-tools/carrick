export async function send(url: string, init: RequestInit): Promise<Response> {
  return fetch(url, init);
}
