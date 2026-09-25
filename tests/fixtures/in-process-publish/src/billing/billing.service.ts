import { DefaultBrokerPublisher } from "../messaging/default-broker-publisher";
import { JobPublisher } from "../jobs/job-publisher";
import { scheduleJob } from "../jobs/schedule";
import { EventPublisher } from "../events/event-publisher";
import { LedgerFeed } from "../events/ledger-feed";
import { FeedService } from "../realtime/feed.service";

export class BillingService {
  constructor(
    private readonly broker: DefaultBrokerPublisher,
    private readonly jobs: JobPublisher,
    private readonly events: EventPublisher,
    private readonly ledger: LedgerFeed,
    private readonly feed: FeedService,
  ) {}

  charge(id: string): void {
    void this.broker.publish("invoice.charged", { id });
    void this.jobs.publish("invoice.queued", { id });
    scheduleJob("invoice.scheduled", { id });
    void this.events.publish("invoice.settled", { id });
    this.ledger.publish("invoice.recorded", { id });
    this.feed.publish({ kind: "invoice.refunded", payload: { id } });
    this.feed.publish({ kind: "invoice.viewed", payload: { id } });
  }
}
