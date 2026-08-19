import { loadHandler } from "@fixture/job-kit";

export const start = async (): Promise<void> => {
  const run = await loadHandler();

  await run();
};
