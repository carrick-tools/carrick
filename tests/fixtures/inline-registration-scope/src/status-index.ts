import { statusRoutes } from './errors';

export async function main(server: any) {
  server.register(statusRoutes, { prefix: '/internal' });
}
