import { QueueClient } from "relay-queue";

export class QueueProvider {
  private static _instance: QueueProvider;

  private _client: QueueClient;

  private constructor(options: { client: QueueClient }) {
    this._client = options.client;
  }

  static getInstance(): void {
    const client = new QueueClient({ region: "eu-west-1" });

    QueueProvider._instance = new QueueProvider({ client });
  }

  async trigger(name: string): Promise<void> {
    await this._client.send({ name });
  }
}
