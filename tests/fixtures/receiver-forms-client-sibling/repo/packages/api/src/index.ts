import mk from '@fixture/mk';
export class A { api = mk.create({ baseURL: 'https://m.test' }); f() { return this.api.get('/p/sib-field'); } }
