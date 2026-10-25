const BASE = "https://api.example.test";

export type SendFn = (input: string, init?: RequestInit) => Promise<Response>;

export class ItemsClient {
  readonly #send: SendFn;

  constructor(options: { send?: SendFn } = {}) {
    this.#send = options.send ?? globalThis.fetch.bind(globalThis);
  }

  list(): Promise<Response> {
    return this.#request("/items");
  }

  #request(path: string): Promise<Response> {
    return this.#send(`${BASE}${path}`, { method: "GET" });
  }
}
