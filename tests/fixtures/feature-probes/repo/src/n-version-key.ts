import { http } from '@fixture/http';
const request = (endpoint: string, options?: { version?: 'v1' | 'v2' }) => {
  const { version = 'v1' } = options || {};
  return http.post(`/${version}${endpoint}`, { body: null });
};
export const a = () => request('/p/n-version-key');
export const b = () => request('/p/n-version-key', { version: 'v2' });
