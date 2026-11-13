import { http } from '@fixture/http';

export class Client {
  constructor(endpoint: string) { http.post(`/v1${endpoint}`, { body: null }); }
}
