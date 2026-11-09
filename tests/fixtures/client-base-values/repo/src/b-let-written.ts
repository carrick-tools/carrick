import mk, { Inst } from '@fixture/mk';
let BASE = 'https://m.test';
export function reset() { BASE = 'https://other.test'; }
const api = mk.create({ baseURL: BASE });
export function f() { return api.get('/p/b-let-written'); }
