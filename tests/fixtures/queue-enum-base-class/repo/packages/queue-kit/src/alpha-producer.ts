import { Topic } from '@fx/topics';
import { ProducerBase } from './producer-base';

export class AlphaProducer extends ProducerBase {
  constructor() {
    super(Topic.Alpha);
  }
}
