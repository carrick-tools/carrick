import { Worker } from 'bullmq';

export class CtorWorker {
  constructor() {
    new Worker('ctor-literal-jobs', async (job) => job.data);
  }
}

export function c2Start() {
  return new CtorWorker();
}
