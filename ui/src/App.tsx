import { useEffect, useRef, useState } from 'react'
import Graph from './Graph'
import { nodes, uploadTo, fanoutHash, getStatus, getStreamUrl } from './api'

/**
 * Converts an image file to JPEG format if it's not already in that format.
 * @param file - The input image file to process
 * @returns A Promise that resolves to a JPEG File object
 */

async function ensureJpeg(file: File): Promise<File> {
  if (file.type === 'image/jpeg' || file.name.toLowerCase().endsWith('.jpg') || file.name.toLowerCase().endsWith('.jpeg')) {
    return file
  }
  const url = URL.createObjectURL(file)
  try {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const i = new Image()
      i.onload = () => resolve(i)
      i.onerror = (e) => reject(e)
      i.src = url
    })
    const canvas = document.createElement('canvas')
    canvas.width = img.naturalWidth || img.width
    canvas.height = img.naturalHeight || img.height
    const ctx = canvas.getContext('2d')
    if (!ctx) throw new Error('canvas 2d context unavailable')
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height)
    const blob: Blob = await new Promise((resolve, reject) => {
      canvas.toBlob(b => b ? resolve(b) : reject(new Error('JPEG conversion failed')), 'image/jpeg', 0.9)
    })
    const name = (file.name.replace(/\.[^.]+$/, '') || 'upload') + '.jpg'
    return new File([blob], name, { type: 'image/jpeg', lastModified: Date.now() })
  } finally {
    URL.revokeObjectURL(url)
  }
}

/**
 * Main application component for the Emerald Content Distribution Lab.
 * Handles file uploads, image processing, and coordinates the distribution process
 * across multiple nodes.
 */
export default function App() {
  const [busy, setBusy] = useState(false)
  const fileRef = useRef<HTMLInputElement>(null)
  const [status, setStatus] = useState<Record<string, any>>({})
  const [errMsg, setErrMsg] = useState<string | null>(null)
  type StepKey = 'prepare' | 'upload' | 'notify' | 'discovery' | 'download' | 'complete'
  const initialSteps: Record<StepKey, { status: 'pending' | 'doing' | 'done' | 'error'; note?: string }> = {
    prepare: { status: 'pending' },
    upload: { status: 'pending' },
    notify: { status: 'pending' },
    discovery: { status: 'pending' },
    download: { status: 'pending' },
    complete: { status: 'pending' },
  }
  const [steps, setSteps] = useState(initialSteps)
  const [current, setCurrent] = useState<{ hash?: string; filename?: string; content_type?: string; provider_node_id?: string; provider?: { id: string } } | null>(null)

  /**
   * Updates the status of a step in the upload/distribution process
   * @param k - The step key to update
   * @param statusVal - The new status value
   * @param note - Optional note to include with the status update
   */
  const mark = (k: StepKey, statusVal: 'pending' | 'doing' | 'done' | 'error', note?: string) => {
    setSteps((prev) => {
      // Don't regress from done/error
      const prevStatus = prev[k]?.status
      if (prevStatus === 'done' || prevStatus === 'error') return prev
      return { ...prev, [k]: { status: statusVal, note } }
    })
  }

  useEffect(() => {
    const id = setInterval(async () => {
      const validNodes = (Array.isArray(nodes) ? nodes : []).filter((n: any) => n && typeof n === 'object' && n.url);
      const ss = await Promise.all(validNodes.map(async n => ({ n, s: await getStatus(n) })))
      const map: Record<string, any> = {}
      ss.forEach(({ n, s }) => (map[n.id] = s))
      setStatus(map)

      // Update checklist inferred steps based on node status and current upload context
      if (current?.hash) {
        const others = nodes.filter((n) => !current.provider || n.id !== current.provider.id)
        const statuses = others.map((n) => map[n.id] || {})
        const discovered = statuses.some((s) => s?.current_hash === current.hash)
        const downloading = statuses.some((s) => s?.current_hash === current.hash && Number(s?.progress || 0) > 0 && !s?.has_image)
        const completedAll = others.length > 0 && others.every((n) => {
          const s = map[n.id] || {}
          return s?.has_image && s?.current_hash === current.hash
        })
        if (discovered) mark('discovery', 'done')
        // Only mark download as done when all peers have the image
        if (completedAll) {
          mark('download', 'done')
          mark('complete', 'done')
        } else if (downloading) {
          mark('download', 'doing')
        }
      }
    }, 500)
    return () => clearInterval(id)
  }, [current])

  /**
   * Handles the file upload process including format conversion and distribution
   */
  const onUpload = async () => {
    const file = fileRef.current?.files?.[0]
    if (!file) return
    setBusy(true)
    setErrMsg(null)
    try {
      setSteps(initialSteps)
      console.log('[prepare] Converting to JPEG if needed…')
      mark('prepare', 'doing')
      const target = nodes[0] // upload to first node by default
      const jpegFile = await ensureJpeg(file)
      mark('prepare', 'done')

      console.log('[upload] Uploading to provider', target)
      mark('upload', 'doing')
      const { hash, filename, content_type, provider_node_id } = await uploadTo(target, jpegFile)
      setCurrent({ hash, filename, content_type, provider_node_id, provider: target })
      mark('upload', 'done')

      console.log('[notify] Fanning out hash to peers', { hash, provider_node_id })
      mark('notify', 'doing')
      await fanoutHash(hash, filename, content_type, provider_node_id, target)
      mark('notify', 'done')
      console.log('[notify] Done')
    } catch (e: any) {
      console.error(e)
      setErrMsg(e?.message || 'Upload failed')
      mark('complete', 'error', e?.message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div style={{ padding: 24, fontFamily: 'ui-sans-serif, system-ui' }}>
      <h1>Emerald Content Distribution Lab</h1>
      <p>Choose a small image and upload. The UI will fan out the <b>hash</b> to all nodes; peers discover the provider and download via P2P.</p>
      <div style={{ display: 'flex', gap: 12, alignItems: 'center' }}>
        <input ref={fileRef} type="file" accept="image/*" />
        <button disabled={busy} onClick={onUpload}>{busy ? 'Uploading…' : 'Upload to node‑a'}</button>
      </div>
      {errMsg && (
        <div style={{ marginTop: 8, color: '#b91c1c', fontSize: 14 }}>{errMsg}</div>
      )}
      <div style={{ marginTop: 16, padding: 12, background: '#fff', border: '1px solid #e5e7eb', borderRadius: 8 }}>
        <div style={{ fontWeight: 600, marginBottom: 8 }}>Checklist</div>
        <ol style={{ margin: 0, paddingLeft: 18, display: 'grid', gap: 6 }}>
          {([
            ['prepare', 'Prepare image (JPEG)'],
            ['upload', 'Upload image to provider'],
            ['notify', 'Notify peers with hash'],
            ['discovery', 'Peers discover provider by hash'],
            ['download', 'Peers download image'],
            ['complete', 'All peers have the image'],
          ] as Array<[StepKey, string]>).map(([k, label]) => {
            const st = steps[k].status
            const icon = st === 'done' ? '✅' : st === 'doing' ? '⏳' : st === 'error' ? '❌' : '▫️'
            return (
              <li key={k} style={{ color: st === 'error' ? '#b91c1c' : '#111827' }}>
                <span style={{ marginRight: 6 }}>{icon}</span>{label}
                {steps[k].note ? <span style={{ color: '#6b7280' }}> — {steps[k].note}</span> : null}
              </li>
            )
          })}
        </ol>
      </div>
      <div style={{ marginTop: 24 }}>
        <Graph status={status} />
      </div>
      <div style={{ marginTop: 24 }}>
        <h2 style={{ marginBottom: 8 }}>Live image stream (progressive)</h2>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 12 }}>
          {nodes.map(n => {
            const s = status[n.id] || {}
            const v = s?.current_hash ? `?v=${s.current_hash}` : (Number.isFinite(s?.progress) ? `?v=${Math.round(s.progress)}` : '')
            const src = `${getStreamUrl(n)}${v}`
            const stripeEntries = s?.stripe_providers && typeof s.stripe_providers === 'object'
              ? Object.entries(s.stripe_providers as Record<string, any>).filter(([, stripes]) => Array.isArray(stripes) && stripes.length > 0)
              : []
            return (
              <div key={n.id} style={{ border: '1px solid #e5e7eb', borderRadius: 8, padding: 8, background: '#fff' }}>
                <div style={{ fontSize: 12, color: '#6b7280', marginBottom: 6 }}>{n.id}{s?.has_image ? '' : ' (no image yet)'}</div>
                {s?.has_image ? (
                  <img src={src} alt={`${n.id} image`} style={{ width: '100%', height: 180, objectFit: 'contain', background: '#fafafa', borderRadius: 4 }} />
                ) : (
                  <div style={{ width: '100%', height: 180, display: 'flex', alignItems: 'center', justifyContent: 'center', color: '#9ca3af', background: '#fafafa', borderRadius: 4 }}>No image</div>
                )}
                {stripeEntries.length > 0 && (
                  <div style={{ marginTop: 8, fontSize: 11, color: '#6b7280', lineHeight: 1.4 }}>
                    <div style={{ fontWeight: 600, marginBottom: 4, color: '#4b5563' }}>Stripe providers</div>
                    <ul style={{ margin: 0, paddingLeft: 16, display: 'grid', gap: 2 }}>
                      {stripeEntries.map(([peer, segments]) => {
                        const labels = (segments as string[]).map((label) => label === 'all' ? 'full blob' : label)
                        return (
                          <li key={`${n.id}-${peer}`}>
                            <span style={{ color: '#111827' }}>{peer}</span>
                            <span style={{ color: '#6b7280' }}> → {labels.join(', ')}</span>
                          </li>
                        )
                      })}
                    </ul>
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
