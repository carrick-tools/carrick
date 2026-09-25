import { AuditTrail } from "../events/audit-trail";
import { FeedService } from "../realtime/feed.service";

const ORDER_AUDITED = "order.audited";

export class AuditService {
  constructor(
    private readonly trail: AuditTrail,
    private readonly feed: FeedService,
  ) {}

  audit(id: string): void {
    this.trail.record("order.traced", { id });
    this.feed.publish({ kind: ORDER_AUDITED, payload: { id } });
    this.feed.publish({ kind: `order.tagged`, payload: { id } });
    void fetch("/audit/ping");
  }
}
