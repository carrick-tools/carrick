export interface Transport {
  publish(topic: string, body: unknown): Promise<void>;
}

export class RelayPublisher {
  constructor(private readonly transport: Transport) {}

  publish(topic: string, body: unknown): Promise<void> {
    return this.transport.publish(topic, body);
  }
}
