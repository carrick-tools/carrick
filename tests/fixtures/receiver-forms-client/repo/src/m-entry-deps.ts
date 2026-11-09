import mk from '@fixture/mk';
const api = mk.create({ baseURL: 'https://m.test' });
export const deps = { client: api };
