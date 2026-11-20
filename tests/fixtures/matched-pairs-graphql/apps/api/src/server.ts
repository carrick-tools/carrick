interface Gadget {
  id: string;
  size: number;
}

export async function getGadget(_req: Request): Promise<Response> {
  const gadget: Gadget = { id: 'g1', size: 3 };
  return new Response(JSON.stringify(gadget));
}
