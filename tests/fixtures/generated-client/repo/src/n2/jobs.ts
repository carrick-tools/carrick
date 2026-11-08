// Negative for the carried-descriptor rule: the same `{ url }` call shape
// over a client whose transport publishes to a broker, with a topic no
// reader folds. The walk reaches a broker end, not an HTTP one, so no HTTP
// row may be read off the object's `url`.
import { bus } from '@fixture/queue';

type Job = { url: string; body?: unknown };
export type Jobs = { post: (job: Job) => Promise<void> };

const topicFor = (job: Job) => job.url.split('/').filter(Boolean).reverse().join('.');

export const createJobs = (): Jobs => {
  const send = async (job: Job) => bus.publish(topicFor(job), job.body);
  return { post: (job) => send(job) } as Jobs;
};

export const jobs = createJobs();

export const enqueueReport = (options?: { jobs?: Jobs; body?: unknown }) =>
  (options?.jobs ?? jobs).post({ url: '/n2/reports', ...options });
