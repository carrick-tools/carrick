// GET /api/v1/widgets/:widgetId — derived from the file name and the exported
// handler, with no model and no installed dependency. `inventory-svc` is the
// consumer.

export type Widget = {
  id: string;
  name: string;
  activeCount: number;
};

export async function loader({
  params,
}: {
  params: { widgetId: string };
}): Promise<Widget> {
  return { id: String(params.widgetId), name: "hex-key", activeCount: 3 };
}
