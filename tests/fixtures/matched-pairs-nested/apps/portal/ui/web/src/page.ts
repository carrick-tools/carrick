import { readGadget, readWidget } from '@shared/widgets';

export async function showWidget(id: string): Promise<string> {
  const widget = await readWidget(id);
  return widget.label;
}

export async function showGadget(id: string): Promise<string> {
  const gadget = await readGadget(id);
  return gadget.id;
}
