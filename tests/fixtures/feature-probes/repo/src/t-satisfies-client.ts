import { http, Client } from '@fixture/http';
const deps = { http } satisfies { http: Client };
export function f() { return deps.http.post('/p/t-satisfies-client', null); }
