import { bySubpath, bySymbol } from "./split";

export const run = async (): Promise<void> => {
  await bySymbol.sessions.create({});
  await bySubpath.sessions.create({});
};
