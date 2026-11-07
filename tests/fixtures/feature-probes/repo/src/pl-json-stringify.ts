import { http } from '@fixture/http';
interface Body { a: number }
declare const body: Body;
export function f() { return http.post('/p/pl-json-stringify', JSON.stringify(body)); }
