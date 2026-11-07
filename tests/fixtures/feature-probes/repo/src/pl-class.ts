import { http } from '@fixture/http';
class Body { a = 1; b = 'x'; }
export function f() { return http.post('/p/pl-class', new Body()); }
