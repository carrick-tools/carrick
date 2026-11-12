import { Client } from "fixture-graph";

export class Mailbox {
  #graph: Client;
  readonly open: Client;

  constructor(token: string) {
    this.#graph = Client.init({ token });
    this.open = Client.init({ token });
  }

  async profile(): Promise<unknown> {
    return this.#graph.api("/me").get();
  }

  async inbox(): Promise<unknown> {
    return this.open.api("/me/messages").get();
  }
}
