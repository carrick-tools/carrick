import { http } from '@fixture/http';
const base = '/n/fallback-stated';
export function f() { return http.post(base ?? '/n/fallback-stated', null); }
