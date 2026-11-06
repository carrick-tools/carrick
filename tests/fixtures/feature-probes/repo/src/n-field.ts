import { http } from '@fixture/http';
export class C { url = '/p/n-field'; go() { return http.post(this.url, null); } }
