import { http } from '@fixture/http';
class Api { c = http; send() { return this.c.post('/p/t-class-field-client', null); } }
export function f() { return new Api().send(); }
