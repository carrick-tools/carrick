import { http, Client } from '@fixture/http';
let c: Client = http;
export function set(x: Client) { c = x; }
export function f() { return c.post('/p/t-let-reassigned', null); }
