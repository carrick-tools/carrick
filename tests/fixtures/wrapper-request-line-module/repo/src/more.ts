import { sendOpt } from './net/opt';
import { Client } from './net/ctor';
import { viaHelper } from './net/helper';

export const opt = sendOpt?.('/opt');
export const client = new Client('/ctor');
[1].forEach(() => viaHelper('/nested'));
