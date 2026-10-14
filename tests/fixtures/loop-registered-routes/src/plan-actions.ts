export const PLAN_ACTIONS = ['create', 'preview', 'apply'] as const;

export type PlanAction = (typeof PLAN_ACTIONS)[number];
