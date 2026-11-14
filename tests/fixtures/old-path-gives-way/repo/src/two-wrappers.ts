import { http } from '@fixture/http';
function sendA(p: string, b: unknown) { return http.post(`https://a.example.test${p}`, b); }
function sendB(p: string, b: unknown) { return http.post(`${process.env.B_BASE}${p}`, b); }
export function f() { return [sendA('/y/a', 1), sendB('/y/b', 2)]; }
