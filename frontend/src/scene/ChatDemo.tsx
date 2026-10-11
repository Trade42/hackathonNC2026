import { BatteryHigh, CaretLeft, CellSignalHigh, Check, Leaf, PersonSimpleWalk, WifiHigh } from '@phosphor-icons/react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useReducedMotion } from '../lib/prefs'
import { Bubble, FoodBubble } from '../ui/Bubble'
import s from './ChatDemo.module.css'

// The scripted group chat that plays on the landing page's phone: Maya heads
// out, Ferry opens a window, you order, the food walks over, CO2 is saved.
// Screen readers get the whole transcript at once; the animation is hidden.

type Who = 'maya' | 'ferry' | 'you' | 'system'
type Msg = { id: string; who: Who; text: string; kind?: 'run' | 'track' | 'co2' }
type Step = { wait: number; typing?: Who; dur?: number; add?: Msg; type?: Msg; reset?: true }

const SCRIPT: Step[] = [
  { wait: 900, add: { id: 'sys', who: 'system', text: 'Maya added Ferry to the group' } },
  { wait: 500, typing: 'maya', dur: 900, add: { id: 'm1', who: 'maya', text: 'heading to Cosmic Cantina at 6, anyone want anything?' } },
  { wait: 400, typing: 'ferry', dur: 1000, add: { id: 'f1', who: 'ferry', kind: 'run', text: 'Ferry to Cosmic Cantina is open until 6:10.' } },
  { wait: 800, type: { id: 'y1', who: 'you', text: 'bean and cheese burrito please' } },
  { wait: 450, typing: 'ferry', dur: 850, add: { id: 'f2', who: 'ferry', text: 'Added. $9.97 is held in your Ferry Wallet.' } },
  { wait: 1400, typing: 'ferry', dur: 700, add: { id: 'f3', who: 'ferry', kind: 'track', text: 'Maya has your food and is walking over. 4 min away.' } },
  { wait: 2600, add: { id: 'f4', who: 'ferry', kind: 'co2', text: 'Delivered to Fetzer Gym.' } },
  { wait: 4200, reset: true },
]

const ALL = SCRIPT.flatMap((st) => (st.add ? [st.add] : st.type ? [st.type] : []))
const NAMES: Record<Who, string> = { maya: 'Maya', ferry: 'Ferry', you: 'You', system: '' }

function MsgBody({ m }: { m: Msg }) {
  if (m.kind === 'run')
    return (
      <>
        <p>{m.text}</p>
        <span className={s.runCard}>
          <FoodBubble size={34} cuisine={['mexican']} name="Cosmic Cantina" />
          <span className={s.runText}>
            <strong>Cosmic Cantina</strong>
            <small>Maya is going, 8 min left</small>
          </span>
        </span>
        <span className={s.fakeBtn}>Add my order</span>
      </>
    )
  if (m.kind === 'track')
    return (
      <>
        <p>{m.text}</p>
        <span className={s.track}>
          <span className={s.trackLine} />
          <span className={s.walkerTrack}>
            <span className={s.walker}>
              <PersonSimpleWalk size={12} weight="bold" />
            </span>
          </span>
        </span>
      </>
    )
  if (m.kind === 'co2')
    return (
      <>
        <p>{m.text}</p>
        <span className={s.co2}>
          <Leaf size={14} weight="fill" />
          +480 g CO2 saved
          {[0, 1, 2, 3, 4].map((k) => (
            <i key={k} style={{ '--k': k } as React.CSSProperties} />
          ))}
        </span>
      </>
    )
  return <p>{m.text}</p>
}

export function ChatDemo() {
  const reduced = useReducedMotion()
  const [msgs, setMsgs] = useState<Msg[]>(reduced ? ALL : [])
  const [typing, setTyping] = useState<Who | null>(null)
  const [draft, setDraft] = useState<{ m: Msg; n: number } | null>(null)
  const [leaving, setLeaving] = useState(false)
  const rootRef = useRef<HTMLDivElement>(null)
  const viewRef = useRef<HTMLDivElement>(null)
  const listRef = useRef<HTMLOListElement>(null)
  const running = useRef(true)

  // Pause while off screen or in a background tab.
  useEffect(() => {
    const el = rootRef.current
    let visible = true
    const update = () => {
      running.current = visible && document.visibilityState === 'visible'
    }
    const io = new IntersectionObserver(([e]) => {
      visible = e.isIntersecting
      update()
    })
    if (el) io.observe(el)
    document.addEventListener('visibilitychange', update)
    return () => {
      io.disconnect()
      document.removeEventListener('visibilitychange', update)
    }
  }, [])

  useEffect(() => {
    if (reduced) {
      setMsgs(ALL)
      setTyping(null)
      setDraft(null)
      return
    }
    let cancelled = false
    // A sleep that only counts down while the demo is visible.
    const sleep = (ms: number) =>
      new Promise<void>((resolve) => {
        let left = ms
        let last = performance.now()
        const tick = () => {
          if (cancelled) return
          const now = performance.now()
          if (running.current) left -= now - last
          last = now
          if (left <= 0) resolve()
          else window.setTimeout(tick, Math.min(left, 120))
        }
        window.setTimeout(tick, Math.min(left, 120))
      })

    setMsgs([])
    void (async () => {
      while (!cancelled) {
        for (const st of SCRIPT) {
          await sleep(st.wait)
          if (cancelled) return
          if (st.reset) {
            setLeaving(true)
            await sleep(600)
            setMsgs([])
            setLeaving(false)
            continue
          }
          if (st.typing) {
            setTyping(st.typing)
            await sleep(st.dur ?? 800)
            setTyping(null)
          }
          if (st.type) {
            const m = st.type
            for (let n = 1; n <= m.text.length && !cancelled; n++) {
              setDraft({ m, n })
              await sleep(34 + Math.random() * 40)
            }
            await sleep(250)
            setDraft(null)
            setMsgs((list) => [...list, m])
          }
          if (st.add) {
            const m = st.add
            setMsgs((list) => [...list, m])
          }
        }
      }
    })()
    return () => {
      cancelled = true
    }
  }, [reduced])

  // Keep the newest message in view, like a chat that scrolls on its own.
  useLayoutEffect(() => {
    const view = viewRef.current
    const list = listRef.current
    if (!view || !list) return
    const over = Math.max(0, list.scrollHeight - view.clientHeight)
    list.style.transform = `translateY(${-over}px)`
  }, [msgs, typing, draft])

  const shown = draft ? [...msgs, draft.m] : msgs

  return (
    <div ref={rootRef} className={s.app}>
      <ol className="sr-only" aria-label="Example group chat with Ferry">
        {ALL.map((m) => (
          <li key={m.id}>
            {m.who === 'system' ? m.text : `${NAMES[m.who]}: ${m.text}${m.kind === 'co2' ? ' Plus 480 grams of CO2 saved.' : ''}`}
          </li>
        ))}
      </ol>
      <div aria-hidden>
        <div className={s.status}>
          <span>6:02</span>
          <span className={s.statusIcons}>
            <CellSignalHigh size={13} weight="fill" />
            <WifiHigh size={13} weight="bold" />
            <BatteryHigh size={15} weight="fill" />
          </span>
        </div>
        <div className={s.header}>
          <CaretLeft size={18} weight="bold" className={s.back} />
          <span className={s.groupAvatar}>
            <Bubble size={34} color="#1E8FD6" seed="hinton">
              <span className={s.groupInitials}>H4</span>
            </Bubble>
          </span>
          <span className={s.groupText}>
            <strong>Hinton 4th floor</strong>
            <small>{typing && typing !== 'you' ? `${NAMES[typing]} is typing…` : 'Maya, Dana, Leo and Ferry'}</small>
          </span>
        </div>
      </div>
      <div ref={viewRef} className={s.view} aria-hidden>
        <ol ref={listRef} className={`${s.list} ${leaving ? s.leaving : ''}`}>
          {shown.map((m) => {
            const isDraft = draft?.m.id === m.id
            if (m.who === 'system')
              return (
                <li key={m.id} className={s.system}>
                  {m.text}
                </li>
              )
            return (
              <li key={m.id} className={`${s.row} ${s[m.who]}`}>
                {m.who !== 'you' && (
                  <span className={s.avatar}>
                    {m.who === 'ferry' ? (
                      <Bubble size={26} color="#1E8FD6" seed="ferry" />
                    ) : (
                      <Bubble size={26} color="#E0683C" seed="maya">
                        <span className={s.initial}>M</span>
                      </Bubble>
                    )}
                  </span>
                )}
                <div className={`${s.msg} ${m.kind ? s[m.kind] : ''}`}>
                  {m.who !== 'you' && <span className={s.name}>{NAMES[m.who]}</span>}
                  {isDraft ? (
                    <p>
                      {m.text.slice(0, draft!.n)}
                      <span className={s.caret} />
                    </p>
                  ) : (
                    <MsgBody m={m} />
                  )}
                  {m.who === 'you' && !isDraft && (
                    <span className={s.sent}>
                      <Check size={11} weight="bold" />
                      <Check size={11} weight="bold" />
                    </span>
                  )}
                </div>
              </li>
            )
          })}
          {typing && typing !== 'you' && (
            <li className={`${s.row} ${s[typing]}`}>
              <span className={s.avatar}>
                <Bubble size={26} color={typing === 'ferry' ? '#1E8FD6' : '#E0683C'} seed={typing}>
                  {typing === 'maya' && <span className={s.initial}>M</span>}
                </Bubble>
              </span>
              <div className={`${s.msg} ${s.typingMsg}`}>
                <span className={s.dots}>
                  <i />
                  <i />
                  <i />
                </span>
              </div>
            </li>
          )}
        </ol>
      </div>
      <div className={s.composer} aria-hidden>
        <span className={s.input}>Message</span>
        <span className={s.send} />
      </div>
    </div>
  )
}
