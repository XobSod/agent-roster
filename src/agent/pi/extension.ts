// agent-roster's extension for pi: reports pi's state to roster, the way hooks
// do for Claude. Roster loads it into the sessions it starts (`pi -e`) and
// rewrites this file when it changes, so edits here don't last. It does
// nothing in a pi that roster didn't start.
import { spawn } from "node:child_process";

export default function (pi: any) {
  const roster = process.env.ROSTER_EXE;
  if (!roster || !process.env.ROSTER_SESSION) return;

  // Each report is a `roster report <kind>` process with the payload on its
  // stdin; one at a time, so they arrive in order.
  let queue: Promise<unknown> = Promise.resolve();
  const report = (kind: string, payload: object = {}) => {
    const body = JSON.stringify(payload);
    queue = queue.then(
      () =>
        new Promise((done) => {
          try {
            const child = spawn(roster, ["report", kind], {
              stdio: ["pipe", "ignore", "ignore"],
            });
            child.on("error", done);
            child.on("close", done);
            child.stdin.on("error", () => {});
            child.stdin.end(body);
          } catch {
            done(undefined);
          }
        }),
    );
  };

  // The conversation (it changes with /new, /resume and /fork), where it's
  // kept, and what it runs on.
  const session = (ctx: any, reason?: string) => ({
    conversation_id: ctx.sessionManager.getSessionId(),
    transcript_path: ctx.sessionManager.getSessionFile() ?? null,
    cleared: reason === "new",
    // pi itself started, not a resume, fork or reload inside it.
    started: reason === "startup",
    model: ctx.model?.id ?? null,
    effort: ctx.thinkingLevel ?? null,
    title: ctx.sessionManager.getSessionName() ?? null,
    context_window: ctx.model?.contextWindow ?? null,
  });

  // Waiting for a prompt.
  pi.on("session_start", (event: any, ctx: any) =>
    report("session", session(ctx, event.reason)),
  );
  // You gave it a task; then it works until it has settled (agent_end can be
  // followed by retries and queued messages).
  pi.on("before_agent_start", (event: any) => {
    if (event.messageType === "user") report("prompt");
  });
  pi.on("agent_start", () => report("working"));
  pi.on("tool_execution_end", () => report("working"));
  pi.on("agent_settled", () => report("idle"));
  // A compaction you asked for (/compact), work outside a turn, till it's
  // done or failed; pi's own run inside a turn.
  pi.on("session_before_compact", (event: any) => {
    if (event.reason === "manual") report("compacting");
  });
  const compacted = (event: any) => {
    if (event.reason === "manual") report("compacted");
  };
  pi.on("session_compact", compacted);
  pi.on("session_compact_failed", compacted);

  // A dialog (an extension asking permission, say) blocking a turn.
  let asking = false;
  pi.on("ui_prompt_start", (_event: any, ctx: any) => {
    if (ctx.isIdle()) return;
    asking = true;
    report("needs-input");
  });
  pi.on("ui_prompt_end", () => {
    if (!asking) return;
    asking = false;
    report("working");
  });

  // Changes that aren't a state: the session's name, its model and thinking.
  pi.on("session_info_changed", (event: any) =>
    report("info", { title: event.name ?? null }),
  );
  pi.on("model_select", (event: any) =>
    report("info", {
      model: event.model?.id ?? null,
      context_window: event.model?.contextWindow ?? null,
    }),
  );
  pi.on("thinking_level_select", (event: any) =>
    report("info", { effort: event.level ?? null }),
  );
}
