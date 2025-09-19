export type NodeInfo = { id: string; url: string }

const DEFAULT_NODES: NodeInfo[] = [
  { id: "node-a", url: "http://localhost:4001" },
  { id: "node-b", url: "http://localhost:4002" },
  { id: "node-c", url: "http://localhost:4003" },
  { id: "node-d", url: "http://localhost:4004" },
  { id: "node-e", url: "http://localhost:4005" },
  { id: "node-f", url: "http://localhost:4006" },
  { id: "node-g", url: "http://localhost:4007" },
  { id: "node-h", url: "http://localhost:4008" },
  { id: "node-i", url: "http://localhost:4009" },
  { id: "node-j", url: "http://localhost:4010" },
];

function normalizeNodes(input: unknown): NodeInfo[] {
  try {
    const raw = typeof input === "string" ? input : JSON.stringify(input ?? "");
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return DEFAULT_NODES;
    const cleaned = parsed
      .filter((x: any) => x && typeof x === "object")
      .map((x: any, i: number) => ({
        id: String(x.id ?? `node-${i + 1}`),
        url: typeof x.url === "string" ? x.url : "",
      }))
      .filter((x) => x.url);
    return cleaned.length ? cleaned : DEFAULT_NODES;
  } catch {
    return DEFAULT_NODES;
  }
}

export const nodes: NodeInfo[] = normalizeNodes(import.meta.env.VITE_NODES_JSON);

export async function getStatus(n: NodeInfo) {
  try {
    const r = await fetch(`${n.url}/status`)
    if (!r.ok) return {}
    return await r.json()
  } catch {
    return {}
  }
}

export function getImageUrl(n: NodeInfo) {
  return `${n.url}/image`
}

export function getStreamUrl(n: NodeInfo) {
  return `${n.url}/image_stream`
}

export async function uploadTo(n: NodeInfo, file: File) {
  const fd = new FormData()
  fd.append('file', file, file.name || 'upload.jpg')
  const r = await fetch(`${n.url}/upload`, { method: 'POST', body: fd, headers: { 'Accept': 'application/json' } })
  if (!r.ok) {
    const msg = await r.text().catch(() => '')
    throw new Error(`upload failed: ${r.status} ${r.statusText}${msg ? ` - ${msg}` : ''}`)
  }
  return r.json() as Promise<{ ticket: string; hash: string; filename: string; content_type: string; provider_node_id?: string }>
}

export async function fanoutTicket(ticket: string, filename: string, content_type: string, except?: NodeInfo) {
  const body = JSON.stringify({ ticket, filename, content_type })
  await Promise.all(nodes.filter(n => n !== except).map(n => fetch(`${n.url}/receive`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body })))
}

export async function fanoutHash(hash: string, filename: string, content_type: string, provider_node_id?: string, except?: NodeInfo) {
  const body = JSON.stringify({ hash, filename, content_type, provider_node_id })
  await Promise.all(nodes.filter(n => n !== except).map(n => fetch(`${n.url}/receive`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body })))
}
