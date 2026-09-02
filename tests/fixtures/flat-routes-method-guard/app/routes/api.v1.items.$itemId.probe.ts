// The only branch is on HEAD, which is the protocol's own read-without-a-body
// rather than a narrowing of what this module serves. With nothing left once
// the protocol verbs are dropped, the module reads as unguarded and the
// convention's default for a write export stands.

type Probe = { id: string };

export async function action({ request }: { request: Request }): Promise<Probe> {
  if (request.method === "HEAD") {
    return { id: "" };
  }
  return { id: "4" };
}
