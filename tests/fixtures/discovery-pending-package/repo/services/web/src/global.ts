const BASE = "https://api.example.test";

export function request(path: string): Promise<Response> {
  return fetch(`${BASE}${path}`);
}

export function load(): Promise<Response> {
  return request("/items");
}
