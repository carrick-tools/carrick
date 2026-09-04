// The verb is a variable, so the module states no method and the convention's
// default stands.
import { createWriteRoute } from "../services/routeBuilders.server";

const configuredMethod = process.env.THING_WRITE_METHOD ?? "PATCH";

const { action } = createWriteRoute(
  {
    params: { thingId: "string" },
    method: configuredMethod,
  },
  async ({ params }) => ({ id: params.thingId })
);

export { action };
