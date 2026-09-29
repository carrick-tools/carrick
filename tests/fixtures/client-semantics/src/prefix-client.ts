import client from "fixture-prefix-http";

export class JobsGateway {
  private readonly jobs = client.create({ prefixUrl: "/svc/jobs/" });

  list(): Promise<unknown> {
    return this.jobs.get("queued");
  }

  run(): Promise<unknown> {
    return this.jobs.post("/run", { json: { op: "start" }, retry: 2 });
  }

  report(): Promise<unknown> {
    return this.jobs("reports", { method: "PUT", json: { period: "day" } });
  }
}
