import { announce } from "./pending";

export function onOrder(b: unknown) {
  announce(b);
}
