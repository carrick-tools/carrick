import { Topic } from './topics';
import { ProducerBase } from './producer-base';

export class AlphaProducer extends ProducerBase {
  constructor() {
    super(Topic.Alpha);
  }
}
