import { http } from '@fixture/http';

export function postOrder(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}

export const submitDraft = () => postOrder('/orders/draft');
