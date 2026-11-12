// A second handler for a documented operation, registered without a scope.
export async function twinRoutes(app: any) {
  app.get('/api/v1/items/:id', async () => ({ id: '2' }));
}
