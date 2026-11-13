import { createSubClient as createPlainClient } from "fixture-sub-plain";
import { createSubClient as createSseClient } from "fixture-sub-sse";
import type { AppRouter } from "../router";

const plain = createPlainClient<AppRouter>({ url: "/api/rpc" });
const sse = createSseClient<AppRouter>({ url: "/api/rpc" });

export async function watchShelfPlain() {
  return plain.shelf.list.subscribe();
}

export async function watchShelfSse() {
  return sse.shelf.list.subscribe();
}
