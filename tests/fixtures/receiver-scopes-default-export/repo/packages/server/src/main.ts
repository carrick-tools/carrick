import server from 'fixture-plugin-server';
import { apiRoutes } from '@fx/routes';

const app = server();

export async function start() {
  app.get('/health', async () => ({ ok: true }));
  await app.register(apiRoutes, {});
  return app;
}
