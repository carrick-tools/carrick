// A generic write handler that narrows the method with a guard: the module
// serves PUT and nothing else. The convention's default for a write export
// (POST) is a phantom here — no such endpoint is served.

type Item = { id: string; name: string };

export async function action({ request }: { request: Request }): Promise<Item> {
  if (request.method !== "PUT") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  const body = (await request.json()) as { name: string };
  return { id: "1", name: body.name };
}
