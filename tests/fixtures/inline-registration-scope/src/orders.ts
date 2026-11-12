import type { App } from 'fixture-plugin-server';

export async function orderRoutes(app: App) {
  app.get('/orders', async () => []);
}
