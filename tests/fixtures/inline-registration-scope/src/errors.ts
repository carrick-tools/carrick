// A handler: it registers nothing on its first parameter, which carries the
// name the router below gives its own.
export function onError(app: any, reply: any) {
  reply.send({ message: app.message });
}

// A router this module also holds, mounted from `status-index.ts`.
export async function statusRoutes(app: any) {
  app.get('/status', async () => ({ ok: true }));
}
