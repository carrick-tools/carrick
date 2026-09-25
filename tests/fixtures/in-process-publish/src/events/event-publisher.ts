export interface OutgoingEvent {
  topic: string;
  body: unknown;
}

/** The in-memory default. */
export class EventPublisher {
  protected readonly published: OutgoingEvent[] = [];

  publish(topic: string, body: unknown): Promise<void> {
    this.published.push({ topic, body });
    return Promise.resolve();
  }
}
