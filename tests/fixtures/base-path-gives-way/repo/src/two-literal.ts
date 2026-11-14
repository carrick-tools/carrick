import { http } from '@fixture/http';
const HOST = 'https://api.example.com';
export function f() { return [http.post(`${HOST}/t/a`, null), http.post(`${HOST}/t/b`, null)]; }
