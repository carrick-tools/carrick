import { http } from '@fixture/http';
declare const process: { env: Record<string, string | undefined> };
export function f() { return http.post(`${process.env.API_URL}/e/one`, null); }
