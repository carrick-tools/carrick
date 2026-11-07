import { Topic } from './topics';
import { ConsumerBase } from './consumer-base';

async function handle(data: unknown) {
  return data;
}

export class GammaConsumer extends ConsumerBase {
  constructor() {
    super(Topic.Gamma);
    this.init(async (job) => handle(job.data));
  }
}
