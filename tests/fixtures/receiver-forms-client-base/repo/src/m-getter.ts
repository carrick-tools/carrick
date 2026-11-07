import mk from '@fixture/mk';
const inst = mk.create({ baseURL: 'https://m.test' });
class A { get api() { return inst; } f() { return this.api.get('/p/m-getter'); } }
export function g() { return new A().f(); }
