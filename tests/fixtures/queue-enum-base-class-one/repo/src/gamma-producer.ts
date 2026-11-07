import { Topic } from './topics';
import { ProducerBase } from './producer-base';

export class GammaProducer extends ProducerBase {
  constructor() {
    super(Topic.Gamma);
  }
}
