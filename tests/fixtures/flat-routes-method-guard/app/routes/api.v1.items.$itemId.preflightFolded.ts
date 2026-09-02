// The same preflight branch written as a call on the member, which the guard
// reader sees through. Seeing through it must not make OPTIONS the served
// verb: the module still serves GET.

type Folded = { items: string[] };

export async function loader({ request }: { request: Request }): Promise<Folded> {
  if (request.method.toUpperCase() === "OPTIONS") {
    return { items: [] };
  }
  return { items: ["three"] };
}
