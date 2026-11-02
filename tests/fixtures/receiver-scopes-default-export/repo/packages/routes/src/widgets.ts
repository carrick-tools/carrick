import type { App } from 'fixture-plugin-server';

export async function widgetRoutes(app: App) {
  app.get('/widgets', async () => ({ widgets: [] as string[] }));
  app.post('/widgets/:widgetId/parts', async () => ({ created: true }));
}
