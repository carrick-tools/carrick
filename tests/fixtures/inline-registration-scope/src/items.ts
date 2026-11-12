import type { App } from 'fixture-plugin-server';

export async function itemRoutes(app: App) {
  app.get('/items', async () => []);
}
