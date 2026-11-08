import { http } from '@fixture/http';
const { p: target, ...others } = { p: '/c/destructure-renamed', q: '/c/other' };
export function f() { return [http.post(target, null), others]; }
