import { HttpEvents } from "./http-events";
import { SocketPublisher } from "./socket-publisher";
import { SwappableFeed } from "./swappable-feed";
import { SyncedStore } from "./synced-store";

export class ShippingService {
  constructor(
    private readonly socket: SocketPublisher,
    private readonly http: HttpEvents,
    private readonly feed: SwappableFeed,
    private readonly store: SyncedStore,
  ) {}

  ship(id: string): void {
    this.socket.publish("shipment.sent", id);
    void this.http.publish("shipment.sent", id);
    this.feed.publish("shipment.sent", { id });
    this.store.publish("shipment.sent", { id });
  }
}
