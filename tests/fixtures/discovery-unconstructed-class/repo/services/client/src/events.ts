import { publish } from "fixture-bus";

export type PublishFn = (topic: string, payload: unknown) => Promise<void>;

export class EventsClient {
  readonly #publish: PublishFn;

  constructor(options: { publish?: PublishFn } = {}) {
    this.#publish = options.publish ?? publish;
  }

  emit(payload: unknown): Promise<void> {
    return this.#publish("item.created", payload);
  }
}
