import { http, Client } from '@fixture/http';
const c = http as Client;
export function f() { return c.post('/p/t-as-cast', null); }
