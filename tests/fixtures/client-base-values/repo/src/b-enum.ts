import mk, { Inst } from '@fixture/mk';
enum Hosts { M = 'https://m.test' }
const api = mk.create({ baseURL: Hosts.M });
export function f() { return api.get('/p/b-enum'); }
