import { http } from '@fixture/http';
function run(fn: (p: string) => unknown) { return fn('/p/t-callback'); }
export function f() { return run((p) => http.post(p, null)); }
