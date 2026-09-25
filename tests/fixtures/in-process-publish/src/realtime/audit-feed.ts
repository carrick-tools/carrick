import { LocalBus } from "./local-bus";

export interface AuditEntry {
  topic: string;
  body: unknown;
}

export class AuditFeed {
  private readonly bus = new LocalBus<AuditEntry>();

  publish(topic: string, body: unknown): void {
    this.bus.next({ topic, body });
  }
}
