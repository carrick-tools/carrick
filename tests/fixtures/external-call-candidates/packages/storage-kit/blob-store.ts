import { VaultClient } from "vault-blob";

export class BlobStore {
  private client: VaultClient;

  constructor(region: string) {
    this.client = new VaultClient({ region });
  }

  async put(key: string, body: string): Promise<void> {
    await this.client.upload(key, body);
  }
}
