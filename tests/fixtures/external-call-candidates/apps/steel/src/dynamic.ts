export const publish = async (payload: unknown): Promise<void> => {
  const { default: Steel } = await import("steel-sdk");
  const client = new Steel({ apiKey: "" });

  await client.sessions.create(payload);
};

export const shutdown = async (): Promise<void> => {
  const sdk = await import("steel-sdk");

  await sdk.close();
};
