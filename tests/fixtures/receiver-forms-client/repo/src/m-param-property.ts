import mk, { Inst } from '@fixture/mk';
class A { constructor(private api: Inst) {} f() { return this.api.get('/p/m-param-property'); } }
export function g() { return new A(mk.create({ baseURL: 'https://m.test' })).f(); }
