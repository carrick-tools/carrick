import mk, { Inst } from '@fixture/mk';
import { CONFIG } from './b-config';
const api = mk.create({ baseURL: CONFIG.base });
export function f() { return api.get('/p/b-obj-entry-import'); }
