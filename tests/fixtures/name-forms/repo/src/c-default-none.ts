import { http } from '@fixture/http';
function send(first: string, p?: string) { return http.post(p, null); }
export function f() { return send('x'); }
