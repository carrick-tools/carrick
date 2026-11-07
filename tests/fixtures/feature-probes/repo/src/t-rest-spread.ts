import { http } from '@fixture/http';
function send(...a: [string, unknown]) { return http.post(...a); }
export function f() { return send('/p/t-rest-spread', null); }
