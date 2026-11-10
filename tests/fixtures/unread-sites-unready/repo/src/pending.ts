import { publish } from "@fixture/pending";

export function announce(body: unknown) {
  publish("orders.created", body);
}
