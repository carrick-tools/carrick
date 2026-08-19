import { render } from "@fixture/crawl-kit/render";

export const snapshot = (url: string): Promise<string> => render(url);
