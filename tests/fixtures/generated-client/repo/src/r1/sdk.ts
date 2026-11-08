import { client } from './client';
import type { Client } from './client';

export const listR1Items = (options?: { client?: Client }) =>
  (options?.client ?? client).get({ url: '/r1/items' });

export const createR1Item = (options: { client?: Client; body: unknown }) =>
  (options.client ?? client).post({ url: '/r1/items', body: options.body });

export const listR1Direct = () => client.get({ url: '/r1/direct' });
