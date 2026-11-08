import { http } from '@fixture/http';
class C { get url() { return '/c/getter'; } go() { return http.post(this.url, null); } }
export function f() { return new C().go(); }
