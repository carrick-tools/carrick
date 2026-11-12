import type { Http } from "./http";

export class OrderClient {
  constructor(private readonly http: Http) {}

  async cancel(id: string): Promise<void> {
    await this.http.post(`/orders/${id}/cancel`, {});
  }
}
