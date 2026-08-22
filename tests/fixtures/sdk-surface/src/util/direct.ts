export function chargeCard(token: string): Promise<unknown> {
  return fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify({ token }),
  });
}
