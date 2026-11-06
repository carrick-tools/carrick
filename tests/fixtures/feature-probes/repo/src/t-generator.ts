import { http } from '@fixture/http';
function* gen(p: string) { yield http.post(p, null); }
export function f() { return gen('/p/t-generator').next(); }
