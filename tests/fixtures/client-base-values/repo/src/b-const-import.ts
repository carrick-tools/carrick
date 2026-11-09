import mk, { Inst } from '@fixture/mk';
import { BASE } from './b-config';
const api = mk.create({ baseURL: BASE });
export function f() { return api.get('/p/b-const-import'); }
