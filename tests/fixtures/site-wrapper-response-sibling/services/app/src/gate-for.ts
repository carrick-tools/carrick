import { makeGate } from "@fixture/net/gate";
export function gateFor() {
  return makeGate({ base: "/inner" });
}
