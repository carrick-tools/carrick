import { sendSingle } from './net/single';
import { runMixed } from './net/mixed';
import { runPaired } from './net/paired';
import { runShared } from './net/shared';
import { runHeldMix } from './net/heldmix';

export function single() {
  return sendSingle('/one');
}
export function mixed() {
  return runMixed('/look');
}
export function paired() {
  return runPaired('/pair');
}
export function shared() {
  return runShared('/open');
}
export function heldMix() {
  return runHeldMix('/job');
}
