import { http } from '@fixture/http';
const ROUTES = { items: '/c/destructure-written/items' };
const { items } = ROUTES;
export function reset() { ROUTES.items = '/elsewhere'; }
export function f() { return http.post(items, null); }
