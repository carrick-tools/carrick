export async function itemDetailRoutes(app: any) {
  app.get('/items/:id', async () => ({ id: '1' }));
}
