import { JobQueue } from "@fixture/jobs";

export class JobPublisher {
  private readonly queue = new JobQueue({ url: "queue:6379" });

  publish(topic: string, body: unknown): Promise<void> {
    return this.queue.add(topic, body);
  }
}
