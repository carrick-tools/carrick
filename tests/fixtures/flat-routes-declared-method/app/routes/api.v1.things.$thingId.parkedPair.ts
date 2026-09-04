// Two handlers taken off one builder result: the same ambiguity the
// destructured pair has, so neither is narrowed.
import { createResourceRoute } from "../services/routeBuilders.server";

const route = createResourceRoute(
  {
    params: { thingId: "string" },
    method: "PUT",
  },
  async ({ params }) => ({ id: params.thingId })
);

export const loader = route.loader;
export const action = route.action;
