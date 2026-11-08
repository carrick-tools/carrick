import { bus } from "@fixture/queue";

function pub(topic: string) {
  return bus.publish(topic, {});
}

export function f() {
  const g = pub.bind(null, "m-bound-publish");
  return g();
}
