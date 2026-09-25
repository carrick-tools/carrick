import { BrokerClient } from "@fixture/broker";
import { CountedPublisher } from "./counted-publisher";

/** What production binds where an EventPublisher is expected. */
export class BrokerEventPublisher extends CountedPublisher {
  private readonly client = new BrokerClient({ servers: ["broker:4222"] });

  override publish(topic: string, body: unknown): Promise<void> {
    return this.client.publish(topic, body);
  }
}
