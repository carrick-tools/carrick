import Forge from "forge-sdk";

export const createForgeClient = (apiKey: string): Forge => {
  const options = { apiKey, timeout: 30000 };

  return new Forge(options);
};
