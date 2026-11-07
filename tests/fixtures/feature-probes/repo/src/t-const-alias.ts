import { http } from '@fixture/http';
const c = http;
export function f() { return c.post('/p/t-const-alias', null); }
