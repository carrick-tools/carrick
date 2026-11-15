import { http } from '@fixture/http';
const LEDGER = process.env.LEDGER_URL;
export function sendConst(endpoint: string) {
  return http.post(`${LEDGER}/v1${endpoint}`, { body: null });
}
