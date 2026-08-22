import { Refunds } from "./refunds.js";

export class Payments {
  refunds: Refunds = new Refunds();

  constructor(private readonly client?: unknown) {}

  create(body: { amount: number }): Promise<unknown>;
  create(body: { amount: number }, idempotencyKey: string): Promise<unknown>;
  create(body: { amount: number }, idempotencyKey?: string): Promise<unknown> {
    return fetch("/v1/payments", {
      method: "POST",
      body: JSON.stringify({ ...body, idempotencyKey }),
    });
  }

  list(): Promise<unknown> {
    return fetch("/v1/payments");
  }
}
