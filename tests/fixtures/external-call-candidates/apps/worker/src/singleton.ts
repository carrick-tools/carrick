import { capture, flush, PulseReporter } from "@fixture/singleton-kit";

export const report = async (name: string): Promise<void> => {
  PulseReporter.start();
  capture(name);
  await flush();
};
