declare function exists(file: string): boolean;
declare function read(file: string): string;

export async function assetRoutes(app: any) {
  for (const [file, type] of [
    ['favicon.svg', 'image/svg+xml'],
    ['logo.png', 'image/png'],
  ] as const) {
    if (exists(file)) {
      app.get(`/${file}`, (_req: any, reply: any) => reply.type(type).send(read(file)));
    }
  }
}
