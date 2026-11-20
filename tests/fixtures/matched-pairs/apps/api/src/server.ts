interface Widget {
  id: string;
  total: number;
}

export async function getWidget(_req: Request): Promise<Response> {
  const widget: Widget = { id: 'w1', total: 3 };
  return new Response(JSON.stringify(widget));
}

export async function getNumericWidget(_req: Request): Promise<Response> {
  const widget: { id: number } = { id: 1 };
  return new Response(JSON.stringify(widget));
}

export async function listWidgets(_req: Request): Promise<Response> {
  const body: { items: Widget[] } = { items: [] };
  return new Response(JSON.stringify(body));
}
