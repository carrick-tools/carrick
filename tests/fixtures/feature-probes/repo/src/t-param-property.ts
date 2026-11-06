import { http, Client } from '@fixture/http';
class Api { constructor(private c: Client) {} send() { return this.c.post('/p/t-param-property', null); } }
export function f() { return new Api(http).send(); }
