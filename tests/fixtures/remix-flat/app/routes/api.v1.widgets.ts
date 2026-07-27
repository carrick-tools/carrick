// The declared-function form of the same convention: it must derive exactly
// like the call-expression form above.

type Widget = { id: string; name: string };

export async function loader(): Promise<Widget[]> {
  return [{ id: "1", name: "hex-key" }];
}

export async function action({ request }: { request: Request }): Promise<Widget> {
  const body = (await request.json()) as { name: string };
  return { id: "2", name: body.name };
}
