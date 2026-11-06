import { http } from '@fixture/http';
export const r = ((p: string) => http.post(p, null))('/p/t-iife');
