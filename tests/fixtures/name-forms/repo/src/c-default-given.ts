import { http } from '@fixture/http';
function send(p = '/c/default-unused') { return http.post(p, null); }
export function f() { return send('/c/default-given'); }
