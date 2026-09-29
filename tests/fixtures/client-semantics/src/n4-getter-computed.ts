import http from "@fixture/http";

const KEY = "baseURL";
const getter = http.create({ baseURL: "/g1", get baseURL() { return "/g2"; } } as any);
const computed = http.create({ baseURL: "/c1", [KEY]: "/c2" });
const strKey = http.create({ "baseURL": "/s1" });
const methodProp = http.create({ baseURL: "/m1", timeout() { return 1; } } as any);

export function n4a(): unknown { return getter.get("/getter"); }
export function n4b(): unknown { return computed.get("/computed"); }
export function n4c(): unknown { return strKey.get("/string-key"); }
export function n4d(): unknown { return methodProp.get("/method-prop"); }
