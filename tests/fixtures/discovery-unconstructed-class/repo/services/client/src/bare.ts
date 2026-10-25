import type { SendFn } from "./items";

export class BareClient {
  readonly #send: SendFn;

  constructor(options: { send: SendFn }) {
    this.#send = options.send;
  }

  get(): Promise<Response> {
    return this.#send("https://api.example.test/bare", { method: "GET" });
  }
}
