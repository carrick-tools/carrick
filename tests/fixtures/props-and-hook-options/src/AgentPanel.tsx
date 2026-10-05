import { useTransport } from "a-ui-runtime";

export function AgentPanel({ endpoint }: { endpoint: string }) {
  const stop = () => fetch(endpoint, { method: "POST", body: "stop" });
  const refresh = () => fetch(`${endpoint}/refresh`, { method: "POST" });
  const page = (section: string, suffix: string) =>
    fetch(`${endpoint}/at${section}${suffix}`);
  const transport = useTransport({
    start: async () => fetch(endpoint, { method: "PUT", body: "start" }),
  });
  return (
    <div>
      <button onClick={stop}>Stop</button>
      <button onClick={refresh}>Refresh</button>
      <button onClick={() => page("/top", "?n=1")}>Top</button>
      <span>{transport.state}</span>
    </div>
  );
}
