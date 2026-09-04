// The result of the builder is exported where it is declared.
import { createWriteRoute } from "../services/routeBuilders.server";

export const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    method: "DELETE",
  },
  async ({ params }) => ({ id: params.thingId })
);
