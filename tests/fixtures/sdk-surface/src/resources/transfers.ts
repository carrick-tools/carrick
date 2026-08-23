import { TransfersApi } from "../api/transfers.js";

export class Transfers {
  constructor(private readonly api: TransfersApi) {}

  send(body: { amount: number }): Promise<unknown> {
    return this.api.send(body);
  }
}
