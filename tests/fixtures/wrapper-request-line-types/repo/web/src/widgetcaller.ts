import { orderDraft, OrderClient, orderAsIs, orderInline } from './net/widgets';
export const a = () => orderDraft({ title: 'x' });
export const b = (c: OrderClient) => c.place({ title: 'y' });
export const c = () => orderAsIs({ body: 'z' });
export const d = () => orderInline({ title: 'w' });
