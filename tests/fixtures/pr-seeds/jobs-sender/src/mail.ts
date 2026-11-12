import { Queue } from "bullmq";

const welcomeQueue = new Queue("welcome-mail");

export async function enqueueWelcome(
  userId: string,
  email: string,
  locale: string,
): Promise<void> {
  await welcomeQueue.add("send", { userId, email, locale });
}
