import { http } from '@fixture/http';
const ROUTES = { items: '/c/destructure-const/items', other: '/c/other' };
const { items } = ROUTES;
export function f() { return http.post(items, null); }
