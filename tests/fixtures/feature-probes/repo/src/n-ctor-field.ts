import { http } from '@fixture/http';
export class C { url: string; constructor() { this.url = '/p/n-ctor-field'; } go() { return http.post(this.url, null); } }
