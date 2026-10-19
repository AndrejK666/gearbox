import React from "@theia/core/shared/react";
import { hasToolCallError } from "@theia/ai-core";
import type { ToolCallChatResponseContent } from "@theia/ai-chat/lib/common";

/** Observe model decisions; approval is exclusively handled by Theia's native renderer. */
export function TitledToolCard({title,response,native,renderResult,cancelled}: {
  title:string; response:ToolCallChatResponseContent;
  native:React.ReactNode; renderResult:()=>React.ReactNode; cancelled:boolean;
}): React.ReactElement {
  const [,update] = React.useReducer((value: number) => value + 1, 0);
  const [decision,setDecision] = React.useState<boolean | undefined>(undefined);
  const [rejected,setRejected] = React.useState(false);
  React.useEffect(() => {
    let active = true;
    const changed = () => { if (active) update(); };
    const subscription = response.onDidChange(changed);
    response.needsUserConfirmation.then(changed, changed);
    response.whenFinished.then(changed, changed);
    response.confirmed.then(value => {if(active)setDecision(value);}, () => {if(active)setRejected(true);});
    return () => {active=false;subscription.dispose();};
  }, [response]);
  const denied = decision === false || (typeof response.result === "object" && response.result !== null && "denied" in response.result && response.result.denied === true);
  const stopped = cancelled || rejected;
  const waiting = response.isAwaitingUserConfirmation && !response.finished && !stopped;
  const status = stopped ? "Cancelled" : denied ? "Declined" : response.finished
    ? (hasToolCallError(response.result) ? "Failed" : "Completed")
    : waiting ? "Confirmation required" : "Working…";
  if (response.finished || denied || stopped) {
    return <details className="gbx-tool-card">
      <summary>{title} · {status}</summary>
      <div>Tool: <code>{response.name}</code></div>
      {response.arguments && <pre>{response.arguments}</pre>}
      {renderResult()}
    </details>;
  }
  // Keep the native component mounted while working so its confirmation tracker,
  // keybindings, timeout and allow/deny actions retain their normal lifecycle.
  return <div className="gbx-tool-card">
    <div>{title} · {status}</div>
    <div hidden={!waiting}>{native}</div>
  </div>;
}
