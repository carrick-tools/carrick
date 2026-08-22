import { createSteelClient } from "./steel";

export const createSession = async (
  key: string,
  args: unknown,
): Promise<unknown> => {
  const steel = createSteelClient(key);

  return steel.sessions.create(args);
};
