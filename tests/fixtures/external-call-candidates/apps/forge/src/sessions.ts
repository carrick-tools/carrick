import { createForgeClient } from "./forge";

export const createSession = async (
  key: string,
  args: unknown,
): Promise<unknown> => {
  const forge = createForgeClient(key);

  return forge.sessions.create(args);
};
