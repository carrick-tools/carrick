// A switch on a case-folded method: the module serves the two verbs it
// branches on, and the convention's default is not one of them.

type Settings = { id: string };

export async function action({ request }: { request: Request }): Promise<Settings> {
  switch (request.method.toUpperCase()) {
    case "PATCH":
      return { id: "1" };
    case "DELETE":
      return { id: "2" };
    default:
      throw new Response("Method Not Allowed", { status: 405 });
  }
}
