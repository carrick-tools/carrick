import { http } from '@fixture/http';
const clients = [http];
export function f() { return clients[0].post('/p/t-array-held', null); }
