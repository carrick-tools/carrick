import { http } from '@fixture/http';

export function sendTpl(endpoint: string) {
  return http.post(`${process.env.LEDGER_URL}/v1${endpoint}`, { body: null });
}
