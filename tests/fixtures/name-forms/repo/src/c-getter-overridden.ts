import { http } from '@fixture/http';
class C { get url() { return '/c/getter-overridden'; } go() { return http.post(this.url, null); } }
class D extends C { get url() { return '/elsewhere'; } }
export function f() { return new D().go(); }
