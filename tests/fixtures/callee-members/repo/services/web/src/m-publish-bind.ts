import { bus } from "@fixture/queue";

const pub = bus.publish.bind(bus);

export function f() {
  return pub("m-publish-bind", {});
}
