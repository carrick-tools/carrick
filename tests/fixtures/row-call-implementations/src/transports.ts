// Stub transports, one per protocol, so no package is needed.
export const http = { post: async (p: string, b: unknown) => 200 };
export const gql = { request: async (q: string) => ({}) };
export const socket = { emit: (e: string) => {} };
export const queue = { publish: (t: string) => {} };
export const rpc = { call: async (n: string) => null };
