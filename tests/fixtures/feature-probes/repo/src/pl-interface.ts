import { http } from '@fixture/http';
interface Body { a: number; b: string }
declare const body: Body;
export function f() { return http.post('/p/pl-interface', body); }
