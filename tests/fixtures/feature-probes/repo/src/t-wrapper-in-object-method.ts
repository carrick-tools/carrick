import { http } from '@fixture/http';
const api = { go(p: string) { return http.post(p, null); } };
export function f() { return api.go('/p/t-wrapper-in-object-method'); }
