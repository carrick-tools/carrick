import { http, Client } from '@fixture/http';
const c: Client | undefined = http;
export function f() { return c!.post('/p/t-non-null', null); }
