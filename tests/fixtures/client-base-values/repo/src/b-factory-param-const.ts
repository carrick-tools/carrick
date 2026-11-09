import mk, { Inst } from '@fixture/mk';
const BASE = 'https://m.test';
function make(base: string) { return mk.create({ baseURL: base }); }
const api = make(BASE);
export function f() { return api.get('/p/b-factory-param-const'); }
