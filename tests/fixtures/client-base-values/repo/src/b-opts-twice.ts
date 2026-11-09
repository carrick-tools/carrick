import mk, { Inst } from '@fixture/mk';
const opts = { baseURL: 'https://m.test' };
const api = mk.create(opts);
const other = mk.create(opts);
export function f() { return api.get('/p/b-opts-twice'); }
export function g() { return other.get('/p/b-opts-twice-other'); }
