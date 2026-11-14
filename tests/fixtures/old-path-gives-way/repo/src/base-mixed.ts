import { http } from '@fixture/http';
const HOST = 'https://api.example.com';
export function f() { return [http.post(`${HOST}/m/a`, null), http.post('/m/b', null)]; }
