import mk from '@fixture/mk';
function getApi() { return mk.create({ baseURL: 'https://m.test' }); }
export function f() { return getApi().get('/p/m-call-result'); }
