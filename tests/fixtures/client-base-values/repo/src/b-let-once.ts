import mk, { Inst } from '@fixture/mk';
let BASE = 'https://m.test';
const api = mk.create({ baseURL: BASE });
export function f() { return api.get('/p/b-let-once'); }
