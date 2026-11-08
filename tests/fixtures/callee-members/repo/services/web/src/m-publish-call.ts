import { bus } from "@fixture/queue";

function pub(topic: string) {
  return bus.publish(topic, {});
}

export function f() {
  return pub.call(null, "m-publish-call");
}
