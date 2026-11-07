import { Topic } from '@fx/topics';
import { ProducerBase } from './producer-base';

export class GammaProducer extends ProducerBase {
  constructor() {
    super(Topic.Gamma);
  }
}
