import { kit } from './kit.ts';
import './parcels/queries.ts';
import './parcels/mutations.ts';

export const schema = kit.toSchema();
