import { client } from '../instance';
import type { Client } from '../client/types';

// The same receiver as `sdk.ts`, called with an object whose only spread
// source reads nothing the resolver states (a parse of stored text): the
// `client` key may hold anything, so the call keeps two values.
export const listStored = (options?: { client?: Client; signal?: AbortSignal }) => {
  return (options?.client ?? client).get({
    url: '/api/v1/stored',
    ...options,
  });
};

export const reloadStored = async (raw: string, signal: AbortSignal) => {
  const { data } = await listStored({ ...JSON.parse(raw), signal });
  return data;
};
