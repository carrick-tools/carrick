import { checkoutClient } from "./registry";

export const release = async (key: string, id: string): Promise<void> => {
  const client = checkoutClient(key);

  await client.sessions.release(id);
};
