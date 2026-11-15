import { http } from '@fixture/http';

export function sendStop(endpoint: string) {
  return http.post(`${process.env.LEDGER_URL}/v1${endpoint}`, { body: null });
}
