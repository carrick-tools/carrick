import { QueueProvider } from "@fixture/jobs-kit/provider";

export const enqueue = (): void => {
  QueueProvider.getInstance();
};
