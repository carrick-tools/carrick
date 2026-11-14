import { http } from "@fixture/http";

function makeWiper(root: string) {
  return () => http.remove<{ up: boolean }>(`${root}/cache`, { headers: {} });
}
export const wipeA = makeWiper("/alpha");
export const wipeB = makeWiper("/beta");
