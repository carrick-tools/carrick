import mk, { Inst } from '@fixture/mk';
function make(base: string) { return mk.create({ baseURL: base }); }
const api = make('https://m.test');
export function f() { return api.get('/p/b-factory-param'); }
