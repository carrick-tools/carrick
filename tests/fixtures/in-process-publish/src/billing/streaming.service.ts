import { StreamBus } from "../events/stream-bus";
import { queueNote } from "../jobs/notes";
import { SnapshotPublisher } from "../jobs/snapshot-publisher";

export class StreamingService {
  constructor(
    private readonly bus: StreamBus,
    private readonly snapshots: SnapshotPublisher,
  ) {}

  stream(id: string): void {
    this.bus.publish("invoice.streamed", { id });
    queueNote("invoice.noted", { id });
    this.snapshots.publish("invoice.snapshotted", { id });
  }
}
