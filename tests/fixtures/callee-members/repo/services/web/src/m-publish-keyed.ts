import { bus } from "@fixture/queue";

const buses = { main: bus };
const k = "main" as const;

export function f() {
  return buses[k].publish("m-publish-keyed", {});
}
