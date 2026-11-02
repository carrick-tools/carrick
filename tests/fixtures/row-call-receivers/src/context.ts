import { Client } from "./client.js";

export class Context {
  async getClient(): Promise<Client> {
    return new Client();
  }
}

export interface Deps {
  client: Client;
  ctx: Context;
}
