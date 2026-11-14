import { http } from '@fixture/http';
function send(p: string, b: unknown) { return http.post(p, b); }
export function f() { return [send('/w/a', 1), send('/w/a', 2)]; }
