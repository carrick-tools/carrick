import { http } from '@fixture/http';
let HOST = 'https://api.example.com';
export function f() { return http.post(`${HOST}/let-host`, null); }
