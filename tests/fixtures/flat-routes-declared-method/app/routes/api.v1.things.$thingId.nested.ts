// A `method` nested inside another option is not the route's verb.
import { createWriteRoute } from "../services/routeBuilders.server";

const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    retry: { method: "DELETE", attempts: 2 },
  },
  async ({ params }) => ({ id: params.thingId })
);

export { action };
