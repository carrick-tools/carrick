import mk, { Inst } from '@fixture/mk';
const config = { timeout: 5 };
config.proxy = false;
const api = mk.create(config);
export function f() { return api.get('/p/b-opts-written'); }
