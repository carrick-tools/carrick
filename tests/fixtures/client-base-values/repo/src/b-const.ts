import mk, { Inst } from '@fixture/mk';
const BASE = 'https://m.test';
const api = mk.create({ baseURL: BASE });
export function f() { return api.get('/p/b-const'); }
