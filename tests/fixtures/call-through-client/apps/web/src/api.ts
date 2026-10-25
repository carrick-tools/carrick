import { getStatus, transport, type Transport } from "@fixture/client";

/** The app's own client: one method per generated request function. */
export class Api {
  constructor(private readonly transport: Transport) {}

  async status() {
    return (await getStatus({ client: this.transport })).data;
  }
}

export function createApi(): Api {
  return new Api(transport);
}
