// skills-analytics — appends a skill-invocation record to the agent-config
// local workspace event log whenever the `skill` tool fires.
//
// Target: ~/.event4u/agent-config/workspace/analytics/events.jsonl
// (the path the /skills discover recommender + /analytics cluster read).
// Record schema confirmed from /analytics show (CSV columns):
//   ts,event,role,task,host_tier,duration_ms
//
// ⚠ UNCONFIRMED TOKENS (contract `docs/contracts/local-analytics.md` is NOT
//   installed on this machine). The event vocabulary is "closed" — emitters
//   reject unknown event names — so these are best-effort placeholders to be
//   reconciled against the contract's § Event vocabulary:
//     - `event` name ("launch")
//     - `role` slug (left null; no `roles.active_role` is configured here)
//     - `host_tier` (left null)

import { appendFile, mkdir } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname } from "node:path";

const EVENTS_PATH =
	process.env.OLLAMA_ROUTER_ANALYTICS_PATH ??
	`${homedir()}/.event4u/agent-config/workspace/analytics/events.jsonl`;

const EVENT_NAME = "launch"; // ⚠ confirm against local-analytics.md § Event vocabulary

// Opt-out: mirror the recommender's contract (env kill-switch).
const OPTED_OUT = process.env.AGENT_CONFIG_NO_LOCAL_ANALYTICS === "1";

// Per-call start timestamps (callID → epoch ms) to compute duration_ms, and a
// dedup set so a double-registered plugin instance never appends twice.
const startTimes = new Map();
const seen = new Set();

async function appendRecord(record) {
	try {
		await mkdir(dirname(EVENTS_PATH), { recursive: true });
		await appendFile(EVENTS_PATH, JSON.stringify(record) + "\n");
	} catch {
		// Best-effort: analytics must never break the tool pipeline.
	}
}

export const id = "skills-analytics";

export const server = async () => ({
	"tool.execute.before": async (input) => {
		if (input.tool !== "skill") return;
		startTimes.set(input.callID, Date.now());
	},
	"tool.execute.after": async (input) => {
		if (input.tool !== "skill") return;
		if (OPTED_OUT) return;
		if (seen.has(input.callID)) return; // idempotent under double registration
		seen.add(input.callID);

		const skill = input.args?.name;
		if (!skill) return;

		const startedAt = startTimes.get(input.callID);
		startTimes.delete(input.callID);

		await appendRecord({
			ts: new Date().toISOString(),
			event: EVENT_NAME,
			role: process.env.OLLAMA_ROUTER_ANALYTICS_ROLE ?? null,
			task: skill,
			host_tier: null,
			duration_ms: startedAt ? Date.now() - startedAt : null,
		});
	},
});

export default { id, server };
