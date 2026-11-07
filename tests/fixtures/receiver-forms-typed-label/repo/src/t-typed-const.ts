import { http, Client } from '@fixture/http';
const c: Client = http;
export function f() { return c.post('/p/t-typed-const', null); }
