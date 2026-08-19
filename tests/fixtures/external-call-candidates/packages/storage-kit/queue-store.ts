import { VaultClient } from "vault-blob";

export class QueueStore {
  readonly client: VaultClient;

  constructor(client: VaultClient) {
    this.client = client;
  }
}

const store = new QueueStore(new VaultClient({ region: "eu-west-1" }));

export const drain = (): Promise<void> => store.client.flush();
