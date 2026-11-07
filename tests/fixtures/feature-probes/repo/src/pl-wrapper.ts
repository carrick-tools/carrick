import { http } from '@fixture/http';
function send<T>(p: string, b: T) { return http.post(p, b); }
export function f() { return send('/p/pl-wrapper', { a: 1 }); }
