import { EventPublisher } from "./event-publisher";

export class CountedPublisher extends EventPublisher {
  count(): number {
    return this.published.length;
  }
}
