// A declared verb outranks a guard in the handler body: the route states PUT,
// and the branch on POST inside is the handler doing something else.
import { createWriteRoute } from "../services/routeBuilders.server";

const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    method: "PUT",
  },
  async ({ request, params }) => {
    if (request.method === "POST") {
      throw new Response("Method Not Allowed", { status: 405 });
    }
    return { id: params.thingId };
  }
);

export { action };
