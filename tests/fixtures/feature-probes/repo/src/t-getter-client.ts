import { http } from '@fixture/http';
class Api { get client() { return http; } send() { return this.client.post('/p/t-getter-client', null); } }
export function f() { return new Api().send(); }
