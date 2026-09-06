// An app-router route module that also calls out to another service. The
// route is declared by file location; the outbound call is the only candidate
// the file offers, so the model's answer for this file arrives anchored on it.

const pricingUrl = "https://pricing.internal/v1/rates";

interface Order {
  id: string;
  total: number;
}

export async function GET(): Promise<Response> {
  const rates = await fetch(pricingUrl);
  const order: Order = { id: "1", total: (await rates.json()) as number };
  return Response.json(order);
}
