import type { Http } from "../api/http";

const BILLING = process.env["BILLING_URL"];

export class BillingClient {
  constructor(private readonly http: Http) {}

  async refund(id: string): Promise<void> {
    await this.http.post(`${BILLING}/events`, { type: "refund", id });
  }
}
