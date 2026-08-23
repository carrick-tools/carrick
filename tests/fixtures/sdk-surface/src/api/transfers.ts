export class TransfersApi {
  send(body: { amount: number }): Promise<unknown> {
    return fetch("/v1/transfers", {
      method: "POST",
      body: JSON.stringify(body),
    });
  }
}
