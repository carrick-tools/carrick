import { AlphaProducer, GammaProducer } from '@fx/queue-kit';

export async function sendAlpha() {
  return new AlphaProducer().add({ id: 1, kind: 'x' });
}

export async function sendGamma() {
  return new GammaProducer().add({ id: 2, kind: 'y' });
}
