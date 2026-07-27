// Flat route module: the path is the filename
// (api.v1.widgets.$widgetId.activate.ts -> /api/v1/widgets/:widgetId/activate)
// and the handler is the RESULT OF A CALL, not a declared function. There is
// no HTTP registration and no path literal anywhere in these bytes, so the
// endpoint can only come from the export's name + the module's location.

import { makeApiRoute } from "~/services/routeBuilders/apiBuilder.server";

type ActivateResponse = { widgetId: string; active: boolean };

const ActivateBody = {
  parse: (input: unknown) => input as { reason: string },
};

export const action = makeApiRoute(
  { body: ActivateBody },
  async ({ params }): Promise<ActivateResponse> => {
    return { widgetId: String(params.widgetId), active: true };
  },
);
