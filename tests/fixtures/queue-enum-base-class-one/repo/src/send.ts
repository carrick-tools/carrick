import { AlphaProducer } from './alpha-producer';
import { GammaProducer } from './gamma-producer';

export async function sendAlpha() {
  return new AlphaProducer().add({ id: 1, kind: 'x' });
}

export async function sendGamma() {
  return new GammaProducer().add({ id: 2, kind: 'y' });
}
