import { createSubClient as createClientV10 } from "fixture-sub10";
import { createSubClient as createClientV11 } from "fixture-sub11";
import type { AppRouter } from "../router";

const older = createClientV10<AppRouter>({ url: "/api/rpc" });
const newer = createClientV11<AppRouter>({ url: "/api/rpc" });

export async function watchShelfV10() {
  return older.shelf.list.subscribe();
}

export async function watchShelfV11() {
  return newer.shelf.list.subscribe();
}
