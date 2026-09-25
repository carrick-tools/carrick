import { BrokerClient } from "@fixture/broker";

export class DefaultBrokerPublisher {
  private readonly client = new BrokerClient();

  publish(topic: string, body: unknown): Promise<void> {
    return this.client.publish(topic, body);
  }
}
