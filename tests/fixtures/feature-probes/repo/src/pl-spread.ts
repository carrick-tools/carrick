import { http } from '@fixture/http';
declare const base: { a: number };
export function f() { return http.post('/p/pl-spread', { ...base, b: 'x' }); }
