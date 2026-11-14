import { save } from "./own-member-lib";
export const saved = () => save("/items", { n: 1 });
