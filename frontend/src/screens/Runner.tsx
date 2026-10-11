import { Camera, CheckCircle, MapPin, Receipt } from '@phosphor-icons/react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Navigate, useParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, upsertOrder, useConfig, useMe, useRun, useRunAction } from '../api/hooks'
import type { Order, RunDetail } from '../api/types'
import { countdown, money } from '../lib/format'
import { runnerPayCents } from '../lib/pricing'
import { useShareLocation } from '../lib/location'
import { uploadReceipt } from '../lib/receipt'
import { useNow } from '../lib/time'
import { forgetRun, rememberRun } from '../state/myRuns'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar, FoodBubble } from '../ui/Bubble'
import { Field } from '../ui/Field'
import { GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import { SwitchRow } from '../ui/Switch'
import c from './common.module.css'
import s from './Runner.module.css'

const statusWord: Record<RunDetail['status'], string> = {
  open: 'Taking orders',
  closed: 'Window closed. Time to buy.',
  purchased: 'Bought. Pick it up.',
  delivering: 'Walking it over',
  delivered: 'All delivered',
  cancelled: 'Cancelled',
}

function ShoppingList({ orders }: { orders: Order[] }) {
  const items = useMemo(() => {
    const m = new Map<string, { name: string; qty: number; who: string[]; notes: string[] }>()
    for (const o of orders)
      for (const l of o.lines) {
        const key = `${l.menu_item_id}|${[...l.choice_ids].sort().join(',')}`
        const cur = m.get(key) ?? { name: l.name, qty: 0, who: [], notes: [] }
        cur.qty += l.qty
        cur.who.push(o.customer.display_name)
        if (l.note) cur.notes.push(`${o.customer.display_name}: ${l.note}`)
        m.set(key, cur)
      }
    return [...m.values()]
  }, [orders])
  if (!items.length) return <p className="muted">No orders yet. Your trip is open to your group; you can also share the link.</p>
  return (
    <ul className={s.list}>
      {items.map((it, i) => (
        <li key={i}>
          <span className={`${s.qty} num`}>{it.qty}×</span>
          <span className={s.itemMain}>
            <strong>{it.name}</strong>
            <span>For {Array.from(new Set(it.who)).join(', ')}</span>
            {it.notes.map((n) => (
              <span key={n} className={s.noteText}>
                “{n}”
              </span>
            ))}
          </span>
        </li>
      ))}
    </ul>
  )
}

function HandoffEntry({ order }: { order: Order }) {
  const qc = useQueryClient()
  const [code, setCode] = useState('')
  const m = useMutation({
    mutationFn: () => api.handoff(order.id, code),
    onSuccess: (o) => {
      upsertOrder(qc, o)
      pop(null, { label: `Delivered to ${order.customer.display_name}`, tone: 'leaf', sound: 'chime' })
    },
  })
  if (['delivered', 'settled'].includes(order.status))
    return (
      <p className={s.done}>
        <CheckCircle size={18} weight="fill" aria-hidden /> Delivered
      </p>
    )
  return (
    <form
      className={s.codeForm}
      onSubmit={(e) => {
        e.preventDefault()
        m.mutate()
      }}
    >
      <Field
        label={`${order.customer.display_name}’s code`}
        inputMode="numeric"
        pattern="[0-9]*"
        maxLength={4}
        value={code}
        onChange={(e) => setCode(e.target.value.replace(/\D/g, ''))}
        error={m.error instanceof ApiError ? m.error.message : undefined}
        autoComplete="off"
      />
      <GelButton type="submit" size="sm" busy={m.isPending} disabled={code.length < 4}>
        Confirm
      </GelButton>
    </form>
  )
}

export default function Runner() {
  const { runId = '' } = useParams()
  const me = useMe()
  const cfg = useConfig()
  const run = useRun(runId)
  const action = useRunAction()
  const qc = useQueryClient()
  const now = useNow(1000)
  const [total, setTotal] = useState('')
  const [photo, setPhoto] = useState<File | null>(null)
  const [uploading, setUploading] = useState(false)
  const [uploadErr, setUploadErr] = useState<string | null>(null)
  const [share, setShare] = useState(false)
  const fileRef = useRef<HTMLInputElement>(null)
  const r = run.data
  const delivering = r?.status === 'delivering'
  const shareState = useShareLocation(runId, share && delivering)

  useEffect(() => {
    if (r) {
      document.title = `Ferrying to ${r.restaurant.name}`
      if (['delivered', 'cancelled'].includes(r.status)) forgetRun(r.id)
      else rememberRun(r.id)
    }
  }, [r])

  useEffect(() => {
    if (delivering && me.data?.profile.share_location_when_running) setShare(true)
  }, [delivering, me.data])

  const preview = useMemo(() => (photo ? URL.createObjectURL(photo) : null), [photo])
  useEffect(() => () => void (preview && URL.revokeObjectURL(preview)), [preview])

  if (run.isPending)
    return (
      <>
        <TopBar title="Captain mode" back="/home" />
        <Page>
          <Loading />
        </Page>
      </>
    )
  if (run.isError || !r)
    return (
      <>
        <TopBar title="Captain mode" back="/home" />
        <Page>
          <Panel>
            <ErrorState error={run.error} onRetry={() => run.refetch()} />
          </Panel>
        </Page>
      </>
    )
  if (me.data && r.runner.id !== me.data.id) return <Navigate to={`/r/${r.id}`} replace />

  const left = Date.parse(r.window_closes_at) - now
  const estimate = r.orders.reduce((a, o) => a + o.subtotal_cents + o.tax_cents, 0)
  const earnings = r.orders.reduce((a, o) => a + runnerPayCents(o.ferry_fee_cents, o.tip_cents), 0)
  const actErr = action.error instanceof ApiError ? action.error.message : action.error ? 'That didn’t go through. Try again.' : null

  const markPaid = async () => {
    setUploadErr(null)
    const cents = Math.round(parseFloat(total) * 100)
    let receipt_path: string | undefined
    if (photo && cfg.data?.supabase) {
      setUploading(true)
      try {
        receipt_path = await uploadReceipt(r.id, photo, cfg.data.supabase)
      } catch (e) {
        setUploading(false)
        setUploadErr(e instanceof Error ? e.message : 'The photo didn’t upload.')
        return
      }
      setUploading(false)
    }
    action.mutate({ runId: r.id, action: 'purchased', receipt_total_cents: cents, receipt_path }, { onSuccess: () => qc.invalidateQueries({ queryKey: keys.run(r.id) }) })
  }

  return (
    <>
      <TopBar title="Captain mode" back="/home" />
      <Page wide>
        <div className={c.split}>
          <div className="stack">
            <Panel className={s.head}>
              <FoodBubble size={64} cuisine={r.restaurant.cuisine} imageUrl={r.restaurant.image_url} name={r.restaurant.name} />
              <div className={s.headText}>
                <h1>Ferrying to {r.restaurant.name}</h1>
                <p className={s.state}>{statusWord[r.status]}</p>
                {r.status === 'open' && (
                  <p className="muted">
                    Window closes in <span className="num">{left > 0 ? countdown(left) : '0:00'}</span>
                  </p>
                )}
              </div>
            </Panel>

            <Panel>
              <h2 className={s.h2}>Shopping list</h2>
              <ShoppingList orders={r.orders} />
              {r.orders.length > 0 && (
                <p className={c.note} style={{ marginTop: 12 }}>
                  Estimated food and tax: <strong className="num">{money(estimate)}</strong>
                </p>
              )}
            </Panel>

            {(r.status === 'delivering' || r.status === 'delivered') && (
              <Panel>
                <h2 className={s.h2}>Drop-offs</h2>
                <ul className={s.stops}>
                  {r.orders.map((o) => (
                    <li key={o.id}>
                      <div className={s.stopHead}>
                        <Avatar user={o.customer} size={36} label={false} />
                        <div>
                          <strong>{o.customer.display_name}</strong>
                          <span>
                            <MapPin size={14} aria-hidden /> {o.dropoff.label}
                          </span>
                        </div>
                      </div>
                      <HandoffEntry order={o} />
                    </li>
                  ))}
                </ul>
              </Panel>
            )}
          </div>

          <div className={c.sticky}>
            <Panel className="stack">
              {r.status === 'open' && (
                <>
                  <h2 className={s.h2}>Ready to go?</h2>
                  <p className="muted">Closing the window locks orders in. Escrow then loads your Ferry Card with {money(estimate)} for the food.</p>
                  <GelButton size="lg" block busy={action.isPending} onClick={() => action.mutate({ runId: r.id, action: 'close' }, { onSuccess: () => pop(null, { label: 'Ferry Card funded', sound: 'drop' }) })}>
                    Close window
                  </GelButton>
                </>
              )}
              {r.status === 'closed' && (
                <>
                  <h2 className={s.h2}>Paid at the counter?</h2>
                  <Field
                    label="Receipt total"
                    inputMode="decimal"
                    placeholder={(estimate / 100).toFixed(2)}
                    value={total}
                    onChange={(e) => setTotal(e.target.value.replace(/[^0-9.]/g, ''))}
                    help="What the Ferry Card paid, tax included."
                  />
                  {cfg.data?.supabase && (
                    <div className={s.photo}>
                      <input ref={fileRef} type="file" accept="image/*" capture="environment" hidden onChange={(e) => setPhoto(e.target.files?.[0] ?? null)} />
                      {preview ? (
                        <button type="button" className={s.thumb} onClick={() => fileRef.current?.click()} aria-label="Retake the receipt photo">
                          <img src={preview} alt="" />
                        </button>
                      ) : null}
                      <GelButton variant="white" size="sm" icon={<Camera size={18} aria-hidden />} onClick={() => fileRef.current?.click()}>
                        {photo ? 'Retake photo' : 'Snap the receipt'}
                      </GelButton>
                      <span className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
                        Optional. People who order can see it.
                      </span>
                    </div>
                  )}
                  {uploadErr && (
                    <p className={c.error} role="alert">
                      {uploadErr}
                    </p>
                  )}
                  <GelButton size="lg" block busy={uploading || action.isPending} disabled={!total || isNaN(parseFloat(total))} icon={<Receipt size={20} aria-hidden />} onClick={markPaid}>
                    Paid
                  </GelButton>
                </>
              )}
              {r.status === 'purchased' && (
                <>
                  <h2 className={s.h2}>Got the bags?</h2>
                  <p className="muted">Tap when you leave the restaurant. Customers see you start walking.</p>
                  <GelButton size="lg" block busy={action.isPending} onClick={() => action.mutate({ runId: r.id, action: 'picked-up' })}>
                    Picked up
                  </GelButton>
                </>
              )}
              {delivering && (
                <>
                  <h2 className={s.h2}>On the way</h2>
                  <SwitchRow
                    label="Share my location"
                    help={
                      shareState === 'denied'
                        ? 'Location is blocked. Allow it in your browser settings to share.'
                        : shareState === 'unsupported'
                          ? 'This device can’t share location.'
                          : shareState === 'on'
                            ? 'Sharing every 5 seconds until you deliver.'
                            : 'Customers see your bubble move on their map.'
                    }
                    checked={share}
                    disabled={shareState === 'unsupported'}
                    onChange={setShare}
                  />
                  <p className="muted">Ask each customer for the 4-digit code on their screen, or they can tap Got it.</p>
                </>
              )}
              {r.status === 'delivered' && (
                <>
                  <h2 className={s.h2}>All delivered</h2>
                  <p>
                    You earned <strong className="num">{money(earnings)}</strong> in base pay and tips. It’s in your Ferry Wallet.
                  </p>
                </>
              )}
              {actErr && (
                <p className={c.error} role="alert">
                  {actErr}
                </p>
              )}
              {r.orders.length > 0 && r.status !== 'delivered' && (
                <p className={c.note}>
                  You’ll earn <strong className="num">{money(earnings)}</strong> in base pay and tips when everything is delivered.
                </p>
              )}
            </Panel>
          </div>
        </div>
      </Page>
    </>
  )
}
