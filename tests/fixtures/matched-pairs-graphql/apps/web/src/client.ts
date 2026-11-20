interface Gadget {
  id: string;
  size: number;
}

export async function loadGadget(): Promise<Gadget> {
  return (await fetch('/g/1')).json();
}
