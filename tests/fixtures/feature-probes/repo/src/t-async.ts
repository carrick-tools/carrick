import { http } from '@fixture/http';
async function send(p: string) { return await http.post(p, null); }
export async function f() { return send('/p/t-async'); }
