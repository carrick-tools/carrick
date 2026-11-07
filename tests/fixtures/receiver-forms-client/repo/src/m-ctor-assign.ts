import mk, { Inst } from '@fixture/mk';
class A { private api: Inst; constructor() { this.api = mk.create({ baseURL: 'https://m.test' }); } f() { return this.api.get('/p/m-ctor-assign'); } }
export function g() { return new A().f(); }
