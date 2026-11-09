import { http } from '@fixture/http';
function send(p: string) { return http.post(p, null); }
const api = { go: send };
export function f() { return api.go('/p/t-entry-own-function'); }
