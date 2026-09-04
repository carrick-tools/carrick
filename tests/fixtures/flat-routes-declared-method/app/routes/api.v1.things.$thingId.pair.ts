// Two handlers destructured from one call. The option cannot say which of them
// it belongs to, so neither is narrowed and both keep the convention's default.
import { createResourceRoute } from "../services/routeBuilders.server";

const { loader, action } = createResourceRoute(
  {
    params: { thingId: "string" },
    method: "PUT",
  },
  async ({ params }) => ({ id: params.thingId })
);

export { loader, action };
