import { http } from '@fixture/http';

type O = { method?: string; body?: unknown };

// The library reads the verb from the `method` key; the client defaults it.
export const send = (url: string, options?: O) => {
  const { method = 'POST', body } = options || {};
  return http.request(url, { method, body });
};
