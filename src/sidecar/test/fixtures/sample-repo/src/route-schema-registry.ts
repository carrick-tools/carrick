/**
 * Schema module for the schema-first route fixture: schema values, the alias
 * a route handler annotates its request with, and the name-to-schema registry
 * the `$ref` lookup is built from — the layout a schema-first service uses,
 * with the schemas in a sibling module to the routes.
 */

import {
  buildSchemaRefs,
  defineSchema,
  type Infer,
  type RouteRequest,
} from './route-schema-lib';

export const CreateWidget = defineSchema<{
  name: string;
  sizeCm: number;
  tags?: string[];
}>();

export const WidgetView = defineSchema<{
  id: string;
  name: string;
  sizeCm: number;
}>();

export const ImportWidgets = defineSchema<{
  sourceUrl: string;
  overwrite: boolean;
}>();

export type CreateWidgetBody = Infer<typeof CreateWidget>;
export type CreateWidgetRequest = RouteRequest<{ Body: CreateWidgetBody }>;

export const { $ref } = buildSchemaRefs({
  CreateWidget,
  WidgetView,
  ImportWidgets,
});
