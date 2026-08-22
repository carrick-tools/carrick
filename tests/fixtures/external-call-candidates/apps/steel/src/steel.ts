import Steel from "steel-sdk";

export const createSteelClient = (apiKey: string): Steel => {
  const options = { apiKey, timeout: 30000 };

  return new Steel(options);
};
