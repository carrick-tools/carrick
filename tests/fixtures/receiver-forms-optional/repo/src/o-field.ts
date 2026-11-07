import { http, Client } from '@fixture/http';
class A { c?: Client = http; s() { return this.c?.post('/p/o-field', null); } }
export function f() { return new A().s(); }
