export type RunSubscribePayload = {
  version: "1";
  runIds: string[];
};

export type RunNotifyPayload = {
  version: "1";
  runId: string;
};

export type ServerToClientEvents = {
  "run:notify": (payload: RunNotifyPayload) => void;
};

export type ClientToServerEvents = {
  "run:subscribe": (payload: RunSubscribePayload) => void;
  "run:unsubscribe": (payload: RunSubscribePayload) => void;
};
