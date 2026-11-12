// Handlers that answer in more than one way behind one route.
export async function reportRoutes(app: any) {
  // The mode is read off the query string: one operation, whatever it sends.
  app.get('/reports/:id', async (request: any) =>
    request.query.mode === 'advanced' ? { id: '1', lines: [] } : { id: '1' },
  );
  // Two routes registered on one line.
  app.get('/reports/:id/lines', async () => []); app.get('/reports/:id/notes', async () => []);
  // The kind is read off a header, which a GET does carry.
  app.get('/feed', async (request: any) =>
    request.headers['x-feed-kind'] === 'full' ? { items: [], total: 0 } : { items: [] },
  );
  // The action is read off the body.
  app.post('/reports', async (request: any) =>
    request.body.action === 'archive' ? { archived: true } : { id: '2' },
  );
}
