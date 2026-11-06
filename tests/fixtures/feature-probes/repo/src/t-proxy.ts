import { http } from '@fixture/http';
const c = new Proxy(http, {});
export function f() { return c.post('/p/t-proxy', null); }
