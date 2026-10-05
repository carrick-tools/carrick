export function NoticePanel(props: { noticesUrl: string }) {
  const dismiss = async (id: string) => {
    await fetch(`${props.noticesUrl}/${id}`, { method: "DELETE" });
  };
  return <button onClick={() => dismiss("n1")}>Dismiss</button>;
}
