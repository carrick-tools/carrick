import { http } from "@fixture/http";

function makePinger(root: string) {
  return () => http.post<{ up: boolean }>(`${root}/ping`);
}
export const pingA = makePinger("/alpha");
export const pingB = makePinger("/beta");
