// The same narrowing through a destructured local: the comparison is against
// a binding initialized from the request's method, not the member expression
// itself. The module serves GET and nothing else.

type Status = { state: string };

export async function action({ request }: { request: Request }): Promise<Status> {
  const { method } = request;
  if (method !== "GET") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return { state: "ready" };
}
