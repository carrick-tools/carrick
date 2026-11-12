import { Worker, type Job } from "bullmq";

export type WelcomeJob = {
  userId: string;
  email: string;
  locale: string;
};

export const welcomeWorker = new Worker(
  "welcome-mail",
  async (job: Job<WelcomeJob>) => {
    void job.data.email;
    void job.data.locale;
  },
);
