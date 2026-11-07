import { http } from '@fixture/http';
enum Root { Api = '/c/enum-template' }
const PART = 'items';
export function f() { return http.post(`${Root.Api}/${PART}`, null); }
