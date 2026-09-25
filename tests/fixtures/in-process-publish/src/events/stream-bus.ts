import { Subject } from "@fixture/streams";

export class StreamBus extends Subject<{ topic: string; body: unknown }> {
  publish(topic: string, body: unknown): void {
    this.next({ topic, body });
  }
}
