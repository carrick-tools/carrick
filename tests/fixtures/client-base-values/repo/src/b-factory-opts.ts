import mk, { Inst } from '@fixture/mk';
function make(o: { baseURL?: string }) { return mk.create(o); }
const api = make({ baseURL: 'https://m.test' });
export function f() { return api.get('/p/b-factory-opts'); }
