// The shape carrick#665 is about: a route builder takes its verb as an option
// and the module exports what the builder returned. The convention's default
// for a write export is POST; this route serves PUT and says so.
import { createWriteRoute } from "../services/routeBuilders.server";

const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    maxContentLength: 1024,
    method: "PUT",
  },
  async ({ params }) => ({ id: params.thingId })
);

export { action };
