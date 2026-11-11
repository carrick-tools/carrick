import { client } from '../instance';
import type { Client } from '../client/types';

// A client that sends nothing.
const offline = {
  get: async (_options: unknown) => ({ data: undefined }),
} as unknown as Client;

// Spread into every call below: it writes `client`.
const base = { client: offline };

// The same receiver as `sdk.ts`, called with an object whose spread source
// writes `client`: the call keeps both values, the module client and the
// one the caller writes.
export const listOffline = (options?: { client?: Client; signal?: AbortSignal }) => {
  return (options?.client ?? client).get({
    url: '/api/v1/offline',
    ...options,
  });
};

export const reloadOffline = async (signal: AbortSignal) => {
  const { data } = await listOffline({ ...base, signal });
  return data;
};
