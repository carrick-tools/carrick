// The handler is a declaration exported under a role name by an export list:
// the comparisons live on the local binding the specifier renames.

type Links = { id: string };

async function linksHandler({ request }: { request: Request }): Promise<Links> {
  if (request.method !== "DELETE" && request.method !== "PATCH") {
    throw new Response("Method Not Allowed", { status: 405 });
  }
  return { id: "1" };
}

export { linksHandler as action };
