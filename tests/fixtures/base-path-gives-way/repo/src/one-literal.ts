import { http } from '@fixture/http';
const HOST = 'https://api.example.com';
export function f() { return http.post(`${HOST}/l/one`, null); }
