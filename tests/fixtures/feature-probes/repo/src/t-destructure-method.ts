import { http } from '@fixture/http';
const { post } = http;
export function f() { return post('/p/t-destructure-method', null); }
