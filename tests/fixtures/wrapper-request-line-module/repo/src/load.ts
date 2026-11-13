import { sendWarm } from './net/warm';
import { sendArg } from './net/arg';
import { sendBoot } from './net/boot';
import { sendTimer } from './net/timer';
import { sendLeft } from './net/left';
import { sendRight } from './net/right';

export const warm = sendWarm('/warm');
export const arg = sendArg(process.argv[2]);
class Boot {
  constructor() {
    sendBoot('/boot');
  }
}
export const started = sendBoot('/started');
export const boot = new Boot();
setTimeout(() => sendTimer('/tick'));
export const timed = sendTimer('/timed');
export const both = [sendLeft('/left'), sendRight('/right')];
