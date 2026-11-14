import { http } from "@fixture/http";

function makeFlush(root: string) {
  return () => http.remove<{ flushed: boolean }>(`${root}/flush`, { headers: {} });
}
export const flushMain = makeFlush("/main");
export function build(maker: (root: string) => () => Promise<{ flushed: boolean }>) { return maker("/other"); }
export const flushOther = build(makeFlush);
