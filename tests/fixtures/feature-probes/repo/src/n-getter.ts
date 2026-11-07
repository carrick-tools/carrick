import { http } from '@fixture/http';
export class C { get url() { return '/p/n-getter'; } go() { return http.post(this.url, null); } }
