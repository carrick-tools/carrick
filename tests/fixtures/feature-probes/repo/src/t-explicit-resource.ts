import { http } from '@fixture/http';
export function f() { using h = { post: http.post.bind(http), [Symbol.dispose]() {} }; return h.post('/p/t-using', null); }
