import { fetchShelfFile } from "../lib/files";
import { COVER_FILE } from "../lib/names";

export const useShelfCover = () => {
  const request = fetchShelfFile(COVER_FILE);
  return request;
};
