import { http, Client } from '@fixture/http';
function send(c: Client, p: string) { return c.post(p, null); }
export function f() { return send(http, '/p/t-param-client'); }
