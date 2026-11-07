import { Worker, Processor } from 'bullmq';
import { Topic } from './topics';

function listen(topic: Topic, p: Processor) {
  return new Worker(topic, p);
}

export function c3Start() {
  return listen(Topic.Alpha, async (job) => job.data);
}
