export async function hooks(app: any, list: Array<{ id: string }>) {
  app.addHook('onRequest', async (req: any) => {
    req.log.info('request');
  });
  app.get('/ids', async () => list.map((x) => x.id));
}
