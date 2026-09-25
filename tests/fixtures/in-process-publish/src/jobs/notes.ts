import * as jobs from "@fixture/jobs";

export function queueNote(topic: string, body: unknown): void {
  jobs.enqueue(topic, body);
}
