import mk, { Inst } from '@fixture/mk';
const api: Inst | undefined = mk.create({ baseURL: 'https://m.test' });
export function f() { return api?.get('/p/m-optional'); }
