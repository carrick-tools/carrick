import { Widget } from "a-widget-kit";
import { AgentPanel } from "./AgentPanel";
import { NoticePanel } from "./NoticePanel";

export function AgentPage({ agentId }: { agentId: string }) {
  return (
    <section>
      <AgentPanel endpoint={`/resources/agents/${agentId}/chat`} />
      <AgentShell chatPath="/resources/agents/main/chat" />
      <NoticePanel noticesUrl="/api/notices" />
      <Widget sourceUrl="/api/widgets" />
    </section>
  );
}

export function AgentShell({ chatPath }: { chatPath: string }) {
  return <AgentPanel endpoint={chatPath} />;
}
