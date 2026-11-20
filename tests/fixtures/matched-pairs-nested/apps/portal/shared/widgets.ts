export interface Widget {
  id: string;
  label: string;
  active: boolean;
}

export async function readWidget(id: string): Promise<Widget> {
  const response = await fetch(`/widgets/${id}`);
  return (await response.json()) as Widget;
}

export interface Gadget {
  id: string;
  meta: unknown;
}

export async function readGadget(id: string): Promise<Gadget> {
  const response = await fetch(`/gadgets/${id}`);
  return (await response.json()) as Gadget;
}
