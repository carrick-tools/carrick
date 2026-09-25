import { Snapshot } from "@fixture/jobs";

export class SnapshotPublisher {
  private readonly taken: string[] = [];

  publish(topic: string, body: unknown): void {
    this.taken.push(topic);
    new Snapshot({ topic, body });
  }
}
