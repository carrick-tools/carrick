import { http } from '@fixture/http';
const deps = { client: http };
const { client } = deps;
export function f() { return client.post('/p/t-destructured-receiver', null); }
