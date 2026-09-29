import http from "@fixture/http";

declare function tweak(o: object): void;
const opts: { timeout?: number; baseURL?: string } = { timeout: 1 };
tweak(opts);
const before = http.create({ baseURL: "/r1", ...opts });
const after = http.create({ ...opts, baseURL: "/r1b" });

export function r1a(): unknown { return before.get("/passed"); }
export function r1b(): unknown { return after.get("/passed-after"); }
