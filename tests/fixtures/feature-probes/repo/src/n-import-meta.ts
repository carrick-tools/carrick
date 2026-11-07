import { http } from '@fixture/http';
export function f() { return http.post(`${(import.meta as any).env.VITE_API}/p/n-import-meta`, null); }
