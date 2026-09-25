import { BrokerClient } from "@fixture/broker";
import { BrokerPublisher } from "../messaging/broker-publisher";
import { RelayPublisher } from "../messaging/relay-publisher";
import { AuditFeed } from "../realtime/audit-feed";
import { FeedService } from "../realtime/feed.service";
import { notify } from "../realtime/notify";

export interface Order {
  id: string;
  total: number;
}

export class OrdersService {
  constructor(
    private readonly feed: FeedService,
    private readonly audit: AuditFeed,
    private readonly broker: BrokerPublisher,
    private readonly relay: RelayPublisher,
    private readonly client: BrokerClient,
  ) {}

  place(order: Order): void {
    this.feed.publish({ kind: "order.placed", payload: { id: order.id } });
    this.broker.publish("order.placed", order);
    this.feed.publish({ kind: "order.totals_changed", payload: { total: order.total } });
    this.feed.publish({ kind: "inventory.reserved", payload: { id: order.id } });
  }

  cancel(id: string): void {
    this.audit.publish("order.cancelled", { id });
    this.relay.publish("order.cancelled", { id });
    this.client.publish("order.archived", { id });
    notify("order.noted", { id });
  }
}
