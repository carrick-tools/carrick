export class Refunds {
  issue(paymentId: string): Promise<unknown> {
    return fetch(`/v1/payments/${paymentId}/refunds`, { method: "POST" });
  }
}
