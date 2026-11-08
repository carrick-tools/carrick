import { client } from './client';
import type { Client } from './client';

export const listR2Items = (options?: { client?: Client }) =>
  (options?.client ?? client).get({ url: '/r2/items' });

export const createR2Item = (options: { client?: Client; body: unknown }) =>
  (options.client ?? client).post({ url: '/r2/items', body: options.body });
