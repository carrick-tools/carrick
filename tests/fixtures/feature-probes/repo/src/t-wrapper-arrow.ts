import { http } from '@fixture/http';
const send = (p: string) => http.post(p, null);
export function f() { return send('/p/t-wrapper-arrow'); }
