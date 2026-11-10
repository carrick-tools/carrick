import { createClient } from "fixture-procs";
import type { AppRouter } from "../router";

const client = createClient<AppRouter>({ url: "/api/rpc" });

export async function listShelf() {
  return client.shelf.list.query();
}

export async function addToShelf(title: string) {
  return client.shelf.add.mutate({ title });
}

export async function innerAndCount() {
  const one = await client.shelf.inner.get.query();
  const [all, count] = await Promise.all([client.shelf.list.query(), client.count.query()]);
  return { one, all, count };
}
