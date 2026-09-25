import { enqueue } from "@fixture/jobs";

export function scheduleJob(topic: string, body: unknown): void {
  enqueue(topic, body);
}
