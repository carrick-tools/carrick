import { createClient } from "fixture-procs";
import type { AppRouter } from "../router";

const client = createClient<AppRouter>({ url: "/api/rpc" });
const loose = createClient<any>({ url: "/api/rpc" });

export async function calls() {
  const list = await client.shelf.list.query();
  // @ts-expect-error shelf.gone is on no router
  const gone = await client.shelf.gone.query();
  // @ts-expect-error shelf.gone is on no router
  const deeper = await client.shelf.gone.deeper.query();
  // @ts-expect-error gone is on no router
  const top = await client.gone.query();
  // @ts-expect-error a query has no mutate
  const wrong = await client.shelf.list.mutate();
  const untyped = await loose.shelf.list.query();
  return { list, gone, deeper, top, wrong, untyped };
}
