import { wire } from "@fixture/wire";

export async function enter(room: string, user: string): Promise<void> {
  await wire.emit(`/${room}/entered`, { headers: { user } });
}
