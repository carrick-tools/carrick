import { http } from '@fixture/http';
function send(p = '/p/n-default-param') { return http.post(p, null); }
export function f() { return send(); }
