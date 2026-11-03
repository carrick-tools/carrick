import type { Store } from "./ports";
import { makeStore } from "./store";

export const shared: Store = makeStore();
