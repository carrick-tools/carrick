declare const process: { env: Record<string, string | undefined> };

// Which services this process runs, read once at startup.
const env = {
  SERVICES: (process.env.SERVICES ?? "web").split(","),
};

export default env;
