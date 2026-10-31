import { send } from "./send.js";

export type Order = {
  id: string;
  total: number;
  currency: string;
};

export class OrdersClient {
  constructor(private readonly baseUrl: string) {}

  readOrder(orderId: string): Promise<Order> {
    const encoded = encodeURIComponent(orderId);
    return send<Order>(`${this.baseUrl}/api/v1/orders/${encoded}`, {
      method: "GET",
    });
  }
}
