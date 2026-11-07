import { http } from '@fixture/http';
interface Body { a: number; b: string }
declare const patch: Partial<Body>;
export function f() { return http.post('/p/pl-generic', patch); }
