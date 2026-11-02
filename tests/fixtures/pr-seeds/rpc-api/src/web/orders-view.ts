import { createClient } from "fixture-procs";
import type { AppRouter } from "../router";

const client = createClient<AppRouter>({ url: "/api/trpc" });

export async function totals(): Promise<number[]> {
  const orders = await client.orders.list.query();
  return orders.map((order) => order.total);
}
