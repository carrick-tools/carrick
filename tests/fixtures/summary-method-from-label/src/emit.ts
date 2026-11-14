import { wire } from "@fixture/wire";

async function join(room: string, user: string): Promise<void> {
  await wire.emit(`/${room}/joined`, { headers: { user } });
}
export const joined = (user: string) => join("lobby", user);
