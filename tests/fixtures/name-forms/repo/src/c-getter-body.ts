import { http } from '@fixture/http';
class C { get url() { const p = '/c/getter-body'; return p; } go() { return http.post(this.url, null); } }
export function f() { return new C().go(); }
