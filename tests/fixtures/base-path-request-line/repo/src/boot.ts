import { sendTpl } from './net/tplbase';
import { sendLit } from './net/litbase';
import { sendLead } from './net/leadbase';

function kinds(): { id: string }[] {
  return JSON.parse(process.argv[2] ?? '[]');
}

export const tpl = sendTpl('/entries');
export const lit = sendLit('/balances');
for (const kind of kinds()) {
  sendLead(kind.id);
}
