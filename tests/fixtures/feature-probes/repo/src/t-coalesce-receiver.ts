import { http, Client } from '@fixture/http';
declare const options: { client?: Client };
export function f() { return (options.client ?? http).post('/p/t-coalesce-receiver', null); }
