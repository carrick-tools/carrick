// One verb served from a branch that returns, the other from the exit a
// negated test guards: both comparisons narrow, whatever their polarity.

type Tags = { id: string };

export async function action({
  request,
  params,
}: {
  request: Request;
  params: { itemId: string };
}): Promise<Tags> {
  if (request.method === "DELETE") {
    return { id: params.itemId };
  }
  if (request.method !== "PUT") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return { id: "put" };
}
