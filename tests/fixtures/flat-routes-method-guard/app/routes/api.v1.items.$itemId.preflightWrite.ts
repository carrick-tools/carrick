// A preflight branch alongside a real narrowing: the handler answers OPTIONS
// and otherwise serves PUT. Only the PUT row is real, and the convention's
// default for a write export (POST) is still not served.

type Written = { id: string };

export async function action({ request }: { request: Request }): Promise<Written> {
  if (request.method === "OPTIONS") {
    return { id: "" };
  }
  if (request.method !== "PUT") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return { id: "1" };
}
