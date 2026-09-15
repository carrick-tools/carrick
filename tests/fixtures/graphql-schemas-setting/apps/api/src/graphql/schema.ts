import { builder } from './builder';
import './widgets/queries';
import './widgets/mutations';

export const schema = builder.toSchema();
