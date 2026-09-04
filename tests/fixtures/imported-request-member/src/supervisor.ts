// A receiver held in a field of an options record and reached off `this`. The
// class imports the client type only, and the call names no path and no verb.
import type { ApiClient } from "./apiClient.js";

type SupervisorOptions = { client: ApiClient; name: string };

export class Supervisor {
  constructor(public readonly options: SupervisorOptions) {}

  async sync() {
    // Neither a path nor a verb is stated here; the member states both.
    return this.options.client.describeSession();
  }
}
