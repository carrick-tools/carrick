import { api } from './nullish';

export async function main(server: any) {
  server.register(api, { prefix: '/root' });
}
