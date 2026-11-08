import { http } from '@fixture/http';
function send(p = '/c/default') { return http.post(p, null); }
export function f() { return send(); }
