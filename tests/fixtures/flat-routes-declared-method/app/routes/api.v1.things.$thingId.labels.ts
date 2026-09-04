// The option accepts a list of verbs, and the route serves each of them.
import { createWriteRoute } from "../services/routeBuilders.server";

const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    method: ["PATCH", "DELETE"],
  },
  async ({ params }) => ({ id: params.thingId })
);

export { action };
