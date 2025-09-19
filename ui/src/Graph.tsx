import ForceGraph2D from 'react-force-graph-2d'
import { nodes, getStreamUrl } from './api'
import { useEffect, useRef, useState } from 'react'

/**
 * A force-directed graph visualization component that displays the network of nodes
 * and their connections in the content distribution system.
 * 
 * @component
 * @param {Object} props - Component props
 * @param {Record<string, any>} props.status - Status information for each node in the network
 */
const DEFAULT_LINKS = [
  { source: 'node-a', target: 'node-b' },
  { source: 'node-a', target: 'node-c' },
  { source: 'node-b', target: 'node-c' },
  { source: 'node-b', target: 'node-d' },
  { source: 'node-c', target: 'node-d' },
  { source: 'node-c', target: 'node-e' },
  { source: 'node-d', target: 'node-e' },
  { source: 'node-d', target: 'node-f' },
  { source: 'node-e', target: 'node-f' },
  { source: 'node-e', target: 'node-g' },
  { source: 'node-f', target: 'node-g' },
  { source: 'node-f', target: 'node-h' },
  { source: 'node-g', target: 'node-h' },
  { source: 'node-g', target: 'node-i' },
  { source: 'node-h', target: 'node-i' },
  { source: 'node-h', target: 'node-j' },
  { source: 'node-i', target: 'node-j' },
]

export default function Graph({ status }: { status: Record<string, any> }) {
  const fgRef = useRef<any>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const [dims, setDims] = useState<{ width: number; height: number }>({ width: 800, height: 500 })
  // Stable graph data reference to prevent re-heat on every status change
  // Seed initial positions at origin-centered circle to ensure immediate visibility
  const initialCount = nodes.length || 1
  const initialRadius = Math.floor(Math.min(dims.width, dims.height) * 0.3)
  const initialSeeded = nodes.map((nn, i) => {
    const a = (i / initialCount) * Math.PI * 2
    return {
      id: nn.id,
      img: undefined as string | undefined,
      progress: 0,
      x: initialRadius * Math.cos(a),
      y: initialRadius * Math.sin(a),
      vx: 0,
      vy: 0,
    }
  })
  const idSet = new Set(nodes.map((n) => n.id))
  const initialLinks = DEFAULT_LINKS.filter(
    (link) => idSet.has(link.source) && idSet.has(link.target),
  )
  const dataRef = useRef<{ nodes: any[]; links: any[] }>({
    nodes: initialSeeded,
    links: initialLinks,
  })
  const imgCache = useRef<Record<string, HTMLImageElement | 'loading' | undefined>>({})
  const storageKey = 'emerald_graph_positions'
  const draggingNode = useRef<any | null>(null)
  const cameraInitRef = useRef(false)

  // Manual drag fallback: if built-in drag doesn't trigger, we drag nearest node by pointer
  useEffect(() => {
    const el = containerRef.current
    const fg = fgRef.current
    if (!el || !fg) return

    const getEventPos = (ev: PointerEvent | MouseEvent) => {
      const rect = el.getBoundingClientRect()
      const x = (ev as PointerEvent).clientX
      const y = (ev as PointerEvent).clientY
      const rx = x - rect.left
      const ry = y - rect.top
      return { x, y, rx, ry }
    }

    const pickNearestNode = (sx: number, sy: number) => {
      let best: any = null
      let bestD = Infinity
      const nodesArr = dataRef.current.nodes
      for (const n of nodesArr) {
        if (typeof n.x !== 'number' || typeof n.y !== 'number') continue
        const { x, y } = fg.graph2ScreenCoords(n.x, n.y) as any
        const dx = x - sx
        const dy = y - sy
        const d = Math.hypot(dx, dy)
        if (d < bestD) { bestD = d; best = n }
      }
      // within ~48px radius of our visual ring
      return bestD <= 50 ? best : null
    }

    const onDown = (ev: PointerEvent) => {
      // ignore clicks on controls
      const target = ev.target as HTMLElement
      if (target && (target.tagName === 'BUTTON' || target.closest('button'))) return
      const { rx, ry } = getEventPos(ev)
      const node = pickNearestNode(rx, ry)
      if (node) {
        draggingNode.current = node
        ;(ev.target as HTMLElement)?.setPointerCapture?.(ev.pointerId)
        ev.preventDefault()
        ev.stopPropagation()
        ;(ev.target as HTMLElement).style.cursor = 'grabbing'
      }
    }

    const onMove = (ev: PointerEvent) => {
      const dn = draggingNode.current
      if (!dn) return
      let { rx, ry } = getEventPos(ev)
      // keep inside canvas in screen space
      const margin = 48
      rx = Math.max(margin, Math.min(dims.width - margin, rx))
      ry = Math.max(margin, Math.min(dims.height - margin, ry))
      const g = fg.screen2GraphCoords(rx, ry) as any
      dn.fx = dn.x = g.x
      dn.fy = dn.y = g.y
      dn.vx = 0; dn.vy = 0
      refreshGraph()
      ev.preventDefault()
      ev.stopPropagation()
    }

    const endDrag = (ev?: PointerEvent) => {
      if (!draggingNode.current) return
      draggingNode.current = null
      savePositions()
      if (ev && ev.pointerId != null) (ev.target as HTMLElement)?.releasePointerCapture?.(ev.pointerId)
      if (ev && ev.target) (ev.target as HTMLElement).style.cursor = 'grab'
    }

    const targets: HTMLElement[] = Array.from(el.querySelectorAll('canvas')) as any
    if (targets.length === 0) targets.push(el)
    const blockWheel = (ev: WheelEvent) => { ev.preventDefault(); ev.stopPropagation() }
    const blockTouchMove = (ev: TouchEvent) => { ev.preventDefault(); ev.stopPropagation() }
    targets.forEach(t => {
      t.addEventListener('pointerdown', onDown)
      t.addEventListener('pointermove', onMove)
      t.addEventListener('pointerup', endDrag)
      t.addEventListener('pointercancel', endDrag)
      t.addEventListener('wheel', blockWheel, { passive: false } as any)
      t.addEventListener('touchmove', blockTouchMove, { passive: false } as any)
      ;(t as HTMLElement).style.cursor = 'grab'
    })
    return () => {
      targets.forEach(t => {
        t.removeEventListener('pointerdown', onDown)
        t.removeEventListener('pointermove', onMove)
        t.removeEventListener('pointerup', endDrag)
        t.removeEventListener('pointercancel', endDrag)
        t.removeEventListener('wheel', blockWheel as any)
        t.removeEventListener('touchmove', blockTouchMove as any)
        ;(t as HTMLElement).style.cursor = ''
      })
    }
  }, [dims.width, dims.height])

  // Observe container size
  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const apply = () => {
      const rect = el.getBoundingClientRect()
      const w = Math.max(320, Math.floor(rect.width))
      const h = Math.max(300, Math.floor(rect.height))
      setDims((d) => (d.width !== w || d.height !== h ? { width: w, height: h } : d))
    }
    apply()
    const ro = new ResizeObserver(() => apply())
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  /**
   * Triggers a graph redraw, handling different versions of react-force-graph
   */
  const refreshGraph = () => {
    const fg = fgRef.current
    if (!fg) return
    if (typeof fg.refresh === 'function') {
      fg.refresh()
      return
    }
    if (typeof fg.tickFrame === 'function') {
      fg.tickFrame()
      return
    }
    // Avoid d3ReheatSimulation here to prevent unwanted motion
  }

  /**
   * Fits the graph view to contain all nodes with optional padding
   * @param {number} ms - Animation duration in milliseconds
   * @param {number} padding - Padding around the nodes in pixels
   */
  const fitToView = (ms: number = 0, padding: number = 80) => {
    const fg = fgRef.current
    if (!fg) return
    try {
      if (typeof fg.zoomToFit === 'function') {
        fg.zoomToFit(ms, padding)
      } else {
        fg.centerAt?.(0, 0, ms)
        fg.zoom?.(1, ms)
      }
    } catch {}
  }

  // One-time: disable forces to keep nodes static
  useEffect(() => {
    const fg = fgRef.current
    if (!fg) return
    try {
      fg.d3Force('link', null)
      fg.d3Force('charge', null)
      fg.d3Force('center', null)
      fg.d3Force('collide', null)
    } catch {}
  }, [])

  // On mount: restore positions from localStorage (if present)
  useEffect(() => {
    try {
      const raw = localStorage.getItem(storageKey)
      if (!raw) return
      const saved: Record<string, { fx: number; fy: number }> = JSON.parse(raw)
      const arr = dataRef.current.nodes
      for (const node of arr) {
        const pos = saved?.[node.id]
        if (pos && Number.isFinite(pos.fx) && Number.isFinite(pos.fy)) {
          node.fx = pos.fx
          node.fy = pos.fy
          node.x = pos.fx
          node.y = pos.fy
          node.vx = 0; node.vy = 0
        }
      }
      // Recenter all saved positions around origin so they are visible without zoom
      let sx = 0, sy = 0, c = 0
      for (const n of arr) {
        if (typeof n.fx === 'number' && typeof n.fy === 'number') { sx += n.fx; sy += n.fy; c++ }
      }
      if (c > 0) {
        const mx = sx / c, my = sy / c
        if (Math.abs(mx) > 1 || Math.abs(my) > 1) {
          for (const n of arr) {
            if (typeof n.fx === 'number' && typeof n.fy === 'number') {
              n.fx -= mx; n.fy -= my
              n.x = n.fx; n.y = n.fy
            }
          }
          savePositions()
        }
      }
      refreshGraph()
      setTimeout(() => fitToView(0, 80), 0)
    } catch {}
  }, [])

  // Redraw after size change without changing zoom
  useEffect(() => {
    if (!dims.width || !dims.height) return
    refreshGraph()
  }, [dims.width, dims.height])

  // One-time: fit all nodes into view after first size
  useEffect(() => {
    if (!dims.width || !dims.height) return
    if (cameraInitRef.current) return
    setTimeout(() => fitToView(0, Math.floor(Math.min(dims.width, dims.height) * 0.05) || 60), 0)
    cameraInitRef.current = true
  }, [dims.width, dims.height])

  // Position nodes deterministically within the canvas
  useEffect(() => {
    const { width, height } = dims
    if (!width || !height) return
    const arr = dataRef.current.nodes
    if (!arr || !arr.length) return
    const cx = 0
    const cy = 0
    const radius = Math.floor(Math.min(width, height) * 0.3)
    const n = arr.length
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2
      // If node has been manually positioned (fx/fy set), keep it
      if (arr[i].fx != null && arr[i].fy != null) {
        arr[i].x = arr[i].fx
        arr[i].y = arr[i].fy
      } else {
        arr[i].x = cx + radius * Math.cos(a)
        arr[i].y = cy + radius * Math.sin(a)
      }
      arr[i].vx = 0; arr[i].vy = 0
    }
    refreshGraph()
  }, [dims.width, dims.height])

  // Update node progress/image from status without replacing graphData
  useEffect(() => {
    for (const node of dataRef.current.nodes) {
      const s = status[node.id] || {}
      node.progress = Math.max(0, Math.min(100, Math.round(s?.progress ?? 0)))
      const info = nodes.find((nn) => nn.id === node.id)
      if (s?.has_image && info) {
        const version = s?.current_hash || (Number.isFinite(s?.progress) ? Math.round(s.progress) : '')
        const base = getStreamUrl(info)
        node.img = version ? `${base}?v=${version}` : base
      } else {
        node.img = undefined
      }
    }
    refreshGraph()
  }, [status])

  const savePositions = () => {
    try {
      const map: Record<string, { fx: number; fy: number }> = {}
      for (const node of dataRef.current.nodes) {
        if (typeof node.fx === 'number' && typeof node.fy === 'number') {
          map[node.id] = { fx: node.fx, fy: node.fy }
        }
      }
      localStorage.setItem(storageKey, JSON.stringify(map))
    } catch {}
  }

  const onResetPositions = () => {
    try { localStorage.removeItem(storageKey) } catch {}
    const { width, height } = dims
    const arr = dataRef.current.nodes
    if (!arr || !arr.length || !width || !height) return
    const cx = 0
    const cy = 0
    const radius = Math.floor(Math.min(width, height) * 0.3)
    const n = arr.length
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2
      delete arr[i].fx; delete arr[i].fy
      arr[i].x = cx + radius * Math.cos(a)
      arr[i].y = cy + radius * Math.sin(a)
      arr[i].vx = 0; arr[i].vy = 0
    }
    refreshGraph()
    setTimeout(() => fitToView(0, 80), 0)
  }

  return (
    <div ref={containerRef} style={{ width: '100%', height: '80vh', position: 'relative', touchAction: 'none', userSelect: 'none', overscrollBehavior: 'contain' }}>
      <div style={{ position: 'absolute', top: 8, right: 8, zIndex: 1 }}>
        <button onClick={onResetPositions} style={{ padding: '6px 10px', fontSize: 12 }}>Reset positions</button>
      </div>
      <ForceGraph2D
        ref={fgRef}
        graphData={dataRef.current}
        warmupTicks={0}
        cooldownTicks={0}
        d3VelocityDecay={1}
        enableNodeDrag={false}
        enableZoomInteraction={false}
        enablePanInteraction={false}
        autoPauseRedraw={false}
        width={dims.width}
        height={dims.height}
        nodePointerAreaPaint={(node: any, color: string, ctx: CanvasRenderingContext2D) => {
          const x = node.x as number, y = node.y as number
          if (!Number.isFinite(x) || !Number.isFinite(y)) return
          // Make the interactive hit area match the visual size (~r=44)
          ctx.fillStyle = color
          ctx.beginPath()
          ctx.arc(x, y, 48, 0, Math.PI * 2, false)
          ctx.fill()
        }}
        nodeCanvasObject={(node: any, ctx) => {
        const x = node.x as number, y = node.y as number
        if (!Number.isFinite(x) || !Number.isFinite(y)) return
        const label = node.id
        const r = 44
        const p = Math.max(0, Math.min(100, node.progress || 0))
        // background ring
        ctx.beginPath()
        ctx.arc(x, y, r, 0, Math.PI * 2)
        ctx.strokeStyle = '#e5e7eb'
        ctx.lineWidth = 4
        ctx.stroke()
        // progress arc
        if (p > 0) {
          const end = -Math.PI / 2 + Math.PI * 2 * (p / 100)
          ctx.beginPath()
          ctx.arc(x, y, r, -Math.PI / 2, end)
          ctx.strokeStyle = p >= 100 ? '#22c55e' : '#3b82f6'
          ctx.lineWidth = 4
          ctx.stroke()
        }
        // cached image (draw only when loaded)
        if (node.img) {
          const url = node.img as string
          const cached = imgCache.current[url]
          if (!cached) {
            const img = new Image()
            img.crossOrigin = 'anonymous'
            img.src = url
            img.onload = () => {
              imgCache.current[url] = img
              refreshGraph()
            }
            img.onerror = () => {
              imgCache.current[url] = undefined
            }
            imgCache.current[url] = 'loading'
          }
          const ready = imgCache.current[url]
          if (ready && ready !== 'loading') {
            ctx.save()
            ctx.beginPath()
            ctx.arc(x, y, 40, 0, Math.PI * 2)
            ctx.clip()
            ctx.drawImage(ready as HTMLImageElement, x - 40, y - 40, 80, 80)
            ctx.restore()
          }
        }
        ctx.font = '12px sans-serif'
        ctx.fillText(label, x + 56, y + 4)
        }}
      />
    </div>
  )
}
