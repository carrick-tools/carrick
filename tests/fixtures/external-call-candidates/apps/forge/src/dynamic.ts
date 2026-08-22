export const publish = async (payload: unknown): Promise<void> => {
  const { default: Forge } = await import("forge-sdk");
  const client = new Forge({ apiKey: "" });

  await client.sessions.create(payload);
};

export const shutdown = async (): Promise<void> => {
  const sdk = await import("forge-sdk");

  await sdk.close();
};
