import mk, { Inst } from '@fixture/mk';
import { BASE as IBASE } from './b-config';
const BASE = 'https://m.test';
const OPTS = { baseURL: 'https://m.test' };
function send(api: Inst) { return api.get('/p/w-opts-const'); }
export function f() { return send(mk.create(OPTS)); }
