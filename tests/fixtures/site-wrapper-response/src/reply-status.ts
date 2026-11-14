import { getStatus } from "./reply-status-lib";
export const status = () => getStatus("/statuses");
