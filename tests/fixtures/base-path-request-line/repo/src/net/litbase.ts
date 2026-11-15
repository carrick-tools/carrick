import { http } from '@fixture/http';
const ROOT = 'http://localhost:7070';
export function sendLit(endpoint: string) {
  return http.post(`${ROOT}/v1${endpoint}`, { body: null });
}
