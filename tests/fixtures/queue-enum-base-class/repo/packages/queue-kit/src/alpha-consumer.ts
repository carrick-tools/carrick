import { Topic } from '@fx/topics';
import { ConsumerBase } from './consumer-base';

async function handle(data: unknown) {
  return data;
}

export class AlphaConsumer extends ConsumerBase {
  constructor() {
    super(Topic.Alpha);
    this.init(async (job) => handle(job.data));
  }
}
