export interface SyncedValue {
  value: unknown;
}

export class SyncedStore {
  constructor(private readonly shared: SyncedValue) {}

  publish(topic: string, body: unknown): void {
    this.shared.value = { topic, body };
  }
}
