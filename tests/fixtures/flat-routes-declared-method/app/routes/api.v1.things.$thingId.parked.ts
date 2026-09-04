// The builder's result is parked on a binding and one handler is taken off it.
// The verb is one hop further away than the destructured form, and it is still
// the route's own statement of its method.
import { createWriteRoute } from "../services/routeBuilders.server";

const route = createWriteRoute(
  {
    params: { thingId: "string" },
    method: "PUT",
  },
  async ({ params }) => ({ id: params.thingId })
);

export const action = route.action;
