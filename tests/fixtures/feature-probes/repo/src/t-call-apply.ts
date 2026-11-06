import { http } from '@fixture/http';
function send(p: string) { return http.post(p, null); }
export function f() { return send.call(null, '/p/t-call-apply'); }
