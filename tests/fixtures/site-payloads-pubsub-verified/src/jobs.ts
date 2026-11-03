import { bus } from "@fixture/bus";

export type Job = { jobId: string; owner: string };

export async function create(jobId: string): Promise<void> {
  await bus.publish("jobs.created", { jobId, priority: 1 });
}

export function watch(): void {
  bus.subscribe("jobs.done", (event: { jobId: string; ok: boolean }) => {
    void event.ok;
  });
}

export async function name(jobId: string, owner: string): Promise<void> {
  const job: Job = { jobId, owner };
  await bus.publish("jobs.named", job);
}
