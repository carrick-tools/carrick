import { vanish } from "@acme/gone";

export function leave(who: string) {
  return vanish(who);
}
