import mk, { Inst } from '@fixture/mk';
const baseURL = 'https://m.test';
const api = mk.create({ baseURL });
export function f() { return api.get('/p/b-shorthand'); }
