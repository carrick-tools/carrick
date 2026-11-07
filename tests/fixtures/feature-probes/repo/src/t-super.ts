import { http } from '@fixture/http';
class Base { send(p: string) { return http.post(p, null); } }
class Sub extends Base { send(p: string) { return super.send(p); } }
export function f() { return new Sub().send('/p/t-super'); }
