// No guard: the handler accepts whatever the framework routes to it, so the
// convention's default for a write export stands.

type Item = { id: string; name: string };

export async function action({ request }: { request: Request }): Promise<Item> {
  const body = (await request.json()) as { name: string };
  return { id: "2", name: body.name };
}
