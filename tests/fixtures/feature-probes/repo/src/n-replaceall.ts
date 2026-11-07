import { http } from '@fixture/http';
declare const id: string;
declare const flag: boolean;
export function f() { return http.post('/p/n-replaceall/:id'.replaceAll(':id', id), null); }
