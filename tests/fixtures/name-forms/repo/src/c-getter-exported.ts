import { http } from '@fixture/http';
export class C { get url() { return '/c/getter-exported'; } go() { return http.post(this.url, null); } }
