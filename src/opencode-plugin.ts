// company-comms for opencode: written by `comms setup`, removed by `comms setup --remove`.
// Reports each session's busy/idle state and what it is doing (its title and todo in progress) to comms, and
// delivers teammates' messages into idle sessions.
import type { Plugin } from "@opencode-ai/plugin"
import { execFile } from "node:child_process"

const COMMS = "__COMMS_EXE__"

const run = (args: string[], cwd: string) =>
  new Promise<string>((resolve) =>
    execFile(COMMS, args, { cwd, timeout: 10_000 }, (err, out) => resolve(err ? "" : String(out).trim())),
  )

export const CompanyComms: Plugin = async ({ client, directory }) => {
  const idle = new Set<string>()
  const subagents = new Set<string>()
  const delivering = new Set<string>()
  const last = new Map<string, { agent?: string; model?: { providerID: string; modelID: string } }>()
  const titles = new Map<string, string>()
  const tasks = new Map<string, string>()
  const doing = (id: string) => {
    const line = [titles.get(id), tasks.get(id)].filter(Boolean).join(" · ")
    void run(["hook", "opencode", "doing", "--session", id, "--doing", line], directory)
  }

  const deliver = async (id: string) => {
    if (delivering.has(id) || subagents.has(id)) return
    delivering.add(id)
    try {
      const text = await run(["hook", "opencode", "poll", "--session", id], directory)
      if (!text) return
      idle.delete(id)
      await client.session.promptAsync({ path: { id }, body: { ...(last.get(id) ?? {}), parts: [{ type: "text", text }] } })
    } finally {
      delivering.delete(id)
    }
  }
  const timer = setInterval(() => idle.forEach((id) => void deliver(id)), 5_000)

  return {
    event: async ({ event }) => {
      const e = event as any
      if (e.type === "session.created" && e.properties?.info?.parentID) subagents.add(e.properties.info.id)
      if (e.type === "message.updated" && e.properties?.info?.role === "user") {
        const m = e.properties.info
        last.set(m.sessionID, { agent: m.agent, model: m.model })
      }
      if (e.type === "session.updated") {
        const { id, title } = e.properties?.info ?? {}
        // Until opencode names it, a session is "New session - <date>".
        if (id && title && !title.startsWith("New session") && titles.get(id) !== title && !subagents.has(id)) {
          titles.set(id, title)
          doing(id)
        }
      }
      if (e.type === "todo.updated") {
        const { sessionID, todos } = e.properties ?? {}
        const now = (todos ?? []).find((t: any) => t.status === "in_progress")?.content ?? ""
        if (sessionID && tasks.get(sessionID) !== now && !subagents.has(sessionID)) {
          tasks.set(sessionID, now)
          doing(sessionID)
        }
      }
      if (e.type === "session.status") {
        const { sessionID, status } = e.properties
        if (subagents.has(sessionID)) return
        void run(["hook", "opencode", "status", "--session", sessionID, "--state", status.type], directory)
        if (status.type === "idle") {
          idle.add(sessionID)
          await deliver(sessionID)
        } else idle.delete(sessionID)
      }
    },
    dispose: async () => clearInterval(timer),
  }
}
