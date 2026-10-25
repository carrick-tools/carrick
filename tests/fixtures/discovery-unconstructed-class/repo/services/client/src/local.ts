import type { SendFn } from "./items";

export class LocalClient {
  readonly #send: SendFn;

  constructor(options: { send?: SendFn } = {}) {
    this.#send = options.send ?? globalThis.fetch.bind(globalThis);
  }

  list(): Promise<Response> {
    return this.#send("https://api.example.test/local", { method: "GET" });
  }
}

async function stub(_input: string): Promise<Response> {
  return new Response("{}");
}

export const local = new LocalClient({ send: stub });
