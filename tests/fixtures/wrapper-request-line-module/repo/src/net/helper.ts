import { sendNested } from './nested';

export function viaHelper(endpoint: string) {
  return sendNested(endpoint);
}
