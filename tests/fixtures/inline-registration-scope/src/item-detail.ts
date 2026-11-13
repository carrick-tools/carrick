import type { App } from 'fixture-plugin-server';

export async function itemDetailRoutes(app: App) {
  app.get('/items/:id', async () => ({ id: '1' }));
}
