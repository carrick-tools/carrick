import { Queue } from 'bullmq';

const literal = new Queue('literal-jobs');

export async function c4Send() {
  return literal.add('job', { n: 4 });
}
