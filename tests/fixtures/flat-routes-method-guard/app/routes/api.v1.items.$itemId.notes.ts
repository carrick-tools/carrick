// A switch on a case-folded method held in a local: the module serves the two
// verbs its cases name, an arrow-const export as well as a declaration.

type Notes = { id: string };

export const action = async ({ request }: { request: Request }): Promise<Notes> => {
  const method = request.method.toUpperCase();
  switch (method) {
    case "DELETE":
      return { id: "1" };
    case "PUT":
      return { id: "2" };
    default:
      throw new Response("Method Not Allowed", { status: 405 });
  }
};
