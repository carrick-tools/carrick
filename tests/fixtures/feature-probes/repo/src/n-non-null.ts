import { http } from '@fixture/http';
const R: { a?: string } = { a: '/p/n-non-null' };
export function f() { return http.post(R.a!, null); }
