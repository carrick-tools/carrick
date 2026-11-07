import { http } from '@fixture/http';
export function f() { return Promise.resolve().then(() => http.post('/p/t-promise-then', null)); }
