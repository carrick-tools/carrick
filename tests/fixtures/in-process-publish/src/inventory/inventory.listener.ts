import { FeedService, type FeedEvent } from "../realtime/feed.service";

export class InventoryListener {
  private reserved = 0;

  constructor(private readonly feed: FeedService) {}

  start(): void {
    this.feed.subscribe("inventory.reserved", (event: FeedEvent) => this.onReserved(event));
    this.feed.subscribe("stock.checked", (event: FeedEvent) => this.onReserved(event));
  }

  private onReserved(event: FeedEvent): void {
    this.reserved += Object.keys(event.payload).length;
  }
}
