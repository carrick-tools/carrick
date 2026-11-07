import { AlphaConsumer } from './alpha-consumer';
import { GammaConsumer } from './gamma-consumer';

export function start() {
  new AlphaConsumer();
  new GammaConsumer();
}
