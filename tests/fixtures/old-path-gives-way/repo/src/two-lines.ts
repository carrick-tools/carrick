import { http } from '@fixture/http';
function send(p: string, b: unknown) { return http.post(p, b); }
export function f() { return send('/z/a', 1); }
export function g() { return send('/z/b', 2); }
