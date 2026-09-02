// The same narrowing written as a call on the member: the handler case-folds
// the method before comparing it, which is still a comparison against the
// method. The module serves PUT and nothing else.

type Archive = { id: string; archived: boolean };

export async function action({ request }: { request: Request }): Promise<Archive> {
  if (request.method.toUpperCase() !== "PUT") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return { id: "1", archived: true };
}
