import mk, { Inst } from '@fixture/mk';
const opts = { timeout: 1 } as { baseURL?: string; timeout: number };
const api = mk.create(opts);
export function f() { return api.get('/p/b-opts-const-nobase'); }
