// A read export beside a write export, only one of which declares its verb.
import { createWriteRoute } from "../services/routeBuilders.server";

export async function loader() {
  return { things: [] };
}

const { action } = createWriteRoute(
  {
    method: "PUT",
  },
  async () => ({ ok: true })
);

export { action };
