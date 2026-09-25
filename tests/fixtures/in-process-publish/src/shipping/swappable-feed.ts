import { Subject } from "@fixture/streams";

export interface Sink {
  next(value: { topic: string; body: unknown }): void;
}

export class SwappableFeed {
  private sink: Sink = new Subject<{ topic: string; body: unknown }>();

  attach(sink: Sink): void {
    this.sink = sink;
  }

  publish(topic: string, body: unknown): void {
    this.sink.next({ topic, body });
  }
}
