// A read handler that answers a CORS preflight inline before doing its real
// work. The OPTIONS branch is protocol plumbing rather than the verb the
// module serves, so the convention's default for a read export (GET) stands.

type Preflight = { items: string[] };

export async function loader({ request }: { request: Request }): Promise<Preflight> {
  if (request.method === "OPTIONS") {
    return { items: [] };
  }
  return { items: ["one", "two"] };
}
