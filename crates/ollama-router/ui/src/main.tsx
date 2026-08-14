import React, { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import './styles.css'

type Node = { id: string; origin: string; healthy: boolean; models: string[]; pressure: string; unhealthy_reason?: string; capacity_error?: string; capacity_source?: string; fail_streak: number; draining: boolean; vram_free_gb?: number; capacity_url_present: boolean }
type Readiness = { ready: boolean; state: string; blockers: { kind: string; severity: string; node_ids: string[]; model_names: string[]; summary: string; action: string }[]; counts: Record<string, number>; recovery?: { id: string; status: string } }
type Job = { id: string; kind: string; status: string; models: string[]; targets: Record<string, { node: string; model: string; status: string; detail?: string }> }

function App() {
  const [token, setToken] = useState('')
  const [draft, setDraft] = useState('')
  const [readiness, setReadiness] = useState<Readiness | null>(null)
  const [nodes, setNodes] = useState<Node[]>([])
  const [jobs, setJobs] = useState<Job[]>([])
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [selectedNode, setSelectedNode] = useState<Node | null>(null)
  const [selectedJob, setSelectedJob] = useState<Job | null>(null)

  async function api(path: string, init: RequestInit = {}) {
    const response = await fetch(path, { ...init, headers: { ...(init.headers || {}), Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' } })
    if (response.status === 401 || response.status === 403) throw new Error('Admin token rejected')
    if (!response.ok) throw new Error(await response.text())
    return response.json()
  }
  async function refresh() {
    if (!token) return
    try {
      const [r, n, j] = await Promise.all([api('/router/v1/readiness'), api('/router/v1/nodes'), api('/router/v1/jobs')])
      setReadiness(r); setNodes(n.nodes); setJobs(j.jobs); setError('')
    } catch (e) { setError(e instanceof Error ? e.message : 'Request failed') }
  }
  useEffect(() => { void refresh(); const id = window.setInterval(() => void refresh(), 5000); return () => window.clearInterval(id) }, [token])
  async function recheck() { setBusy(true); try { const r = await api('/router/v1/readiness/recheck', { method: 'POST' }); setReadiness(r); await refresh() } catch (e) { setError(e instanceof Error ? e.message : 'Recheck failed') } finally { setBusy(false) } }
  async function ensure(model: string) { setBusy(true); try { await api('/router/v1/models/ensure', { method: 'POST', body: JSON.stringify({ models: [model] }) }); await refresh() } catch (e) { setError(e instanceof Error ? e.message : 'Ensure failed') } finally { setBusy(false) } }
  async function reload() { setBusy(true); try { await api('/router/v1/reload', { method: 'POST', body: '{}' }); await refresh() } catch (e) { setError(e instanceof Error ? e.message : 'Reload failed') } finally { setBusy(false) } }

  if (!token) return <main className="unlock"><section className="card"><p className="eyebrow">OLLAMA ROUTER</p><h1>Fleet readiness console</h1><p>Use the router admin bearer token. It stays in memory and is cleared when this tab closes.</p><form onSubmit={(e) => { e.preventDefault(); setToken(draft.trim()) }}><label>Admin token<input autoFocus type="password" value={draft} onChange={(e) => setDraft(e.target.value)} /></label><button>Unlock console</button></form></section></main>
  const status = readiness?.ready ? 'ready' : readiness?.state === 'recovering' ? 'recovering' : 'action_required'
  const desired = [...new Set(nodes.flatMap(n => n.models))]
  return <main className="shell"><header><div><p className="eyebrow">CONTROL PLANE / {new Date().toLocaleTimeString()}</p><h1>Fleet readiness</h1></div><div className="actions"><button className="secondary" onClick={() => { setToken(''); setDraft('') }}>Lock</button><button onClick={() => void recheck()} disabled={busy}>Recheck fleet</button></div></header>
    <section className={`banner ${status}`}><div><span className="status-dot" /><strong>{status === 'ready' ? 'Ready for inference' : status === 'recovering' ? 'Recovery in progress' : 'Action required'}</strong><p>{readiness?.ready ? `${readiness.counts.healthy} healthy node(s) can serve clients.` : readiness?.blockers[0]?.summary || 'Loading fleet diagnostics…'}</p></div><span className="badge">{readiness ? `${readiness.counts.healthy}/${readiness.counts.total} healthy` : 'loading'}</span></section>
    {error && <div role="alert" className="error">{error}</div>}
    <div className="grid"><section className="card wide"><div className="section-title"><h2>What is blocking readiness?</h2><span>{readiness?.blockers.length || 0} blockers</span></div>{readiness?.blockers.length ? <div className="blockers">{readiness.blockers.map((b, i) => <article className="blocker" key={`${b.kind}-${i}`}><div><span className={`severity ${b.severity}`} /> <strong>{b.summary}</strong><p>{b.action}</p>{b.node_ids.length > 0 && <small>Nodes: {b.node_ids.join(', ')}</small>}</div>{b.model_names.map(m => <button className="secondary" key={m} onClick={() => void ensure(m)}>Ensure {m}</button>)}</article>)}</div> : <p className="empty">No blockers. The endpoint is ready to serve Ollama clients.</p>}{readiness?.counts.total === 0 && <div className="gitops"><strong>GitOps instruction</strong><code>id: gpu-1{`\n`}url: http://node-agent-host:11434{`\n`}capacity_url: http://node-agent-host:11436</code><p>Add this to fleet.yaml, start the node-agent, then reload. The console never writes permanent inventory.</p><button className="secondary" onClick={() => void reload()} disabled={busy}>Reload inventory</button></div>}</section>
      <section className="card"><div className="section-title"><h2>Recovery</h2><span>live</span></div>{readiness?.recovery ? <div className="timeline"><Step label="Provisioning" done={readiness.recovery.status !== 'pending'} /><Step label="Enrollment" done={readiness.recovery.status === 'running'} /><Step label="Health probe" done={false} /><Step label="Ready" done={false} /></div> : <p className="empty">No active recovery operation.</p>}</section>
      <section className="card wide"><div className="section-title"><h2>Node inventory</h2><span>{nodes.length} nodes</span></div><div className="table-wrap"><table><thead><tr><th>Node</th><th>Health</th><th>Reason</th><th>Pressure</th><th>VRAM free</th><th>Models</th></tr></thead><tbody>{nodes.map(n => <tr key={n.id} onClick={() => setSelectedNode(n)}><td><strong>{n.id}</strong><small>{n.origin}</small></td><td><span className={`pill ${n.healthy ? 'ok' : 'bad'}`}>{n.healthy ? 'healthy' : 'unhealthy'}</span></td><td>{n.unhealthy_reason || '—'}</td><td>{n.pressure}</td><td>{n.vram_free_gb == null ? 'unknown' : `${n.vram_free_gb.toFixed(1)} GiB`}</td><td>{n.models.length}</td></tr>)}</tbody></table></div></section>
      <section className="card"><div className="section-title"><h2>Model coverage</h2><span>desired / present</span></div>{desired.length ? desired.map(model => <div className="model" key={model}><span>{model}</span><span className="muted">{nodes.filter(n => n.models.includes(model)).length} nodes</span><button className="secondary" onClick={() => void ensure(model)}>Ensure</button></div>) : <p className="empty">No models discovered yet.</p>}</section>
      <section className="card"><div className="section-title"><h2>Operations</h2><span>{jobs.length}</span></div>{jobs.slice(0, 5).map(j => <button className="job" key={j.id} onClick={() => setSelectedJob(j)}><span className={`pill ${j.status === 'success' ? 'ok' : j.status === 'failed' ? 'bad' : 'warn'}`}>{j.status}</span><span>{j.kind} · {j.models.join(', ') || 'recovery'}</span></button>)}{!jobs.length && <p className="empty">No model operations recorded.</p>}</section>
    </div>{selectedNode && <dialog open className="drawer"><button className="close" onClick={() => setSelectedNode(null)}>×</button><p className="eyebrow">NODE DETAILS</p><h2>{selectedNode.id}</h2><p>Health: {selectedNode.healthy ? 'healthy' : selectedNode.unhealthy_reason || 'unhealthy'}</p><p>Capacity source: {selectedNode.capacity_source || 'unknown'}</p><p>Agent: {selectedNode.capacity_url_present ? 'configured' : 'unavailable'}</p><p>Fail streak: {selectedNode.fail_streak}</p><p>Models: {selectedNode.models.join(', ') || 'none'}</p></dialog>}{selectedJob && <dialog open className="drawer"><button className="close" onClick={() => setSelectedJob(null)}>×</button><p className="eyebrow">JOB DETAILS</p><h2>{selectedJob.kind} · {selectedJob.status}</h2>{Object.values(selectedJob.targets).map(t => <div className="job" key={`${t.node}-${t.model}`}><span>{t.node} / {t.model}</span><span>{t.status}</span></div>)}</dialog>}</main>
}
function Step({ label, done }: { label: string; done: boolean }) { return <div className={`step ${done ? 'done' : ''}`}><span />{label}</div> }
createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>)
