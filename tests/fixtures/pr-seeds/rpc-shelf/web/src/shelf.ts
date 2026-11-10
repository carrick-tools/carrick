import { createClient } from "fixture-procs";
import type { AppRouter } from "../../api/src/router";

const client = createClient<AppRouter>({ url: "/api/rpc" });

export async function load() {
  const all = await client.shelf.list.query();
  const one = await client.shelf.get.query();
  // @ts-expect-error shelf.gone is not served
  const gone = await client.shelf.gone.query();
  return { all, one, gone };
}
