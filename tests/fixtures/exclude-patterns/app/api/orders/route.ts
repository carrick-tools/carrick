import { firstOrder } from "../../../lib/orders";

export async function GET() {
  return Response.json(firstOrder([{ id: "o1", total: 3 }]));
}
