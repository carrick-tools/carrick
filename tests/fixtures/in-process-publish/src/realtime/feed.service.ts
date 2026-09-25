import { Subject } from "@fixture/streams";
import { filter } from "@fixture/streams/operators";

export interface FeedEvent {
  kind: string;
  payload: Record<string, unknown>;
}

export class FeedService {
  private readonly events = new Subject<FeedEvent & { at: number }>();

  publish(event: FeedEvent): void {
    this.events.next({ ...event, at: Date.now() });
  }

  subscribe(kind: string, handler: (event: FeedEvent) => void) {
    return this.events.pipe(filter((event: FeedEvent) => event.kind === kind)).subscribe(handler);
  }
}
