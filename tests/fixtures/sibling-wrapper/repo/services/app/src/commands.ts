import { getHealth } from "@fixture/client";
import { ItemsApi } from "./api";

export async function show(id: string) {
  const api = new ItemsApi();
  return api.get(id);
}

export async function report() {
  const api = new ItemsApi();
  return api.status();
}

export async function both() {
  const api = new ItemsApi();
  return api.both();
}

/** A command that calls the sibling's function itself. */
export async function health() {
  return getHealth();
}
