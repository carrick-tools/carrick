import { client } from './instance';
import type { Client, RequestOptions } from './client/types';

export type Options<TData> = Omit<RequestOptions, 'url' | 'method'> & TData & { client?: Client; meta?: Record<string, unknown> };

export const listGadgets = (options?: Options<{ query?: { page?: string } }>) => {
  return (options?.client ?? client).get({
    url: '/api/v1/gadgets',
    ...options,
  });
};

export const getGadget = (options: Options<{ path: { id: string } }>) => {
  return (options.client ?? client).get({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/api/v1/gadgets/{id}',
    ...options,
  });
};

export const createGadget = (options: Options<{ body: { name: string } }>) => {
  return (options.client ?? client).post({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/api/v1/gadgets',
    ...options,
    headers: { 'Content-Type': 'application/json', ...options.headers },
  });
};
