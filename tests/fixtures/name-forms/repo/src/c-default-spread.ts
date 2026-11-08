import { http } from '@fixture/http';
function send(first: string, p = '/c/default-spread') { return http.post(p, null); }
export function f(args: [string]) { return send(...args); }
