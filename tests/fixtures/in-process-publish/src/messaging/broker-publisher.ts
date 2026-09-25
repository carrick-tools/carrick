import { BrokerClient } from "@fixture/broker";

export class BrokerPublisher {
  private readonly client = new BrokerClient({ servers: ["broker:4222"] });

  publish(topic: string, body: unknown): Promise<void> {
    return this.client.publish(topic, body);
  }
}
