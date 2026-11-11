import { client } from './instance';
import type { Client } from './client/types';

// The same receiver as `sdk.ts`, called from callbacks a library runs. The
// spread sources are the callbacks' own parameters (`key[0]`, `fnOptions`),
// which nothing in this repository passes: they stay unread, so the call
// keeps two values.
export const listWidgets = (options?: { client?: Client; signal?: AbortSignal }) => {
  return (options?.client ?? client).get({
    url: '/api/v1/widgets',
    ...options,
  });
};

export const widgetOptions = () => ({
  queryFn: async ({ queryKey: key, signal }: { queryKey: Array<Record<string, unknown>>; signal: AbortSignal }) => {
    const { data } = await listWidgets({ ...key[0], signal });
    return data;
  },
  mutationFn: async (fnOptions: { client?: Client }) => {
    const { data } = await listWidgets({ ...fnOptions });
    return data;
  },
});
