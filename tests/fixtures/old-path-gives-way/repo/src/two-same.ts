import { http } from '@fixture/http';
function send(p: string, b: unknown) { return http.post(p, b); }
export function f() { return [send('/x/a', 1), send('/x/b', 2)]; }
