import mk from '@fixture/mk';
class A { api = mk.create({ baseURL: 'https://m.test' }); f() { return this.api.get('/p/m-field-init'); } }
export function g() { return new A().f(); }
