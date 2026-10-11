import { ArrowLeft, ChatCircleText, MapPin } from '@phosphor-icons/react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AnimatePresence, motion, MotionConfig } from 'motion/react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, useMe, usePlaces, useRestaurants } from '../api/hooks'
import type { Place, Profile, UsualTime, WalkingPace, Weekday } from '../api/types'
import { money } from '../lib/format'
import { walkMinutes } from '../lib/geocode'
import { safeNext } from '../lib/next'
import { CAMPUS_CENTER, knownPlaces } from '../lib/places'
import { useReducedMotion } from '../lib/prefs'
import { LazyMap } from '../map/LazyMap'
import { notifications } from '../platform'
import { Chip, chipGroupClass } from '../ui/Chip'
import { Field } from '../ui/Field'
import { GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { Sheet } from '../ui/Sheet'
import { Loading } from '../ui/States'
import { glassClass, Panel } from '../ui/Surface'
import { SwitchRow } from '../ui/Switch'
import { WaterGlass } from '../ui/WaterGlass'
import s from './Onboarding.module.css'

const STEPS = ['name', 'dropoff', 'diet', 'favorites', 'times', 'ferrying', 'reach', 'finish'] as const
// How far you'd walk from where you head back to, to hand food to customers.
const REACH = [150, 300, 500, 800, 1200]
const DIETS = ['Vegetarian', 'Vegan', 'Halal', 'Kosher', 'Gluten-free', 'Dairy-free']
const AVOIDS = ['Peanuts', 'Tree nuts', 'Shellfish', 'Pork', 'Spicy', 'Mushrooms']
const CUISINES = ['Mexican', 'American', 'Vietnamese', 'Mediterranean', 'Burgers', 'Bagels', 'Dessert', 'Coffee', 'Thai', 'Indian']
const MEALS = [
  { label: 'Breakfast', start: '08:00', end: '10:00' },
  { label: 'Lunch', start: '12:00', end: '14:00' },
  { label: 'Dinner', start: '18:00', end: '20:00' },
  { label: 'Late night', start: '22:00', end: '23:59' },
]
const DAYSETS: { key: string; label: string; days: Weekday[] }[] = [
  { key: 'week', label: 'Weekdays', days: ['mon', 'tue', 'wed', 'thu', 'fri'] },
  { key: 'end', label: 'Weekends', days: ['sat', 'sun'] },
  { key: 'all', label: 'Every day', days: ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'] },
]
const PACES: { key: WalkingPace; label: string; help: string }[] = [
  { key: 'relaxed', label: 'Relaxed', help: 'About 20 min a mile' },
  { key: 'normal', label: 'Normal', help: 'About 17 min a mile' },
  { key: 'brisk', label: 'Brisk', help: 'About 15 min a mile' },
]

type Draft = { name: string; profile: Profile; dayset: string }
const DRAFT_KEY = 'ferry.onboarding'

function loadDraft(): Draft | null {
  try {
    return JSON.parse(sessionStorage.getItem(DRAFT_KEY) ?? 'null')
  } catch {
    return null
  }
}

const toggle = <T,>(list: T[], v: T) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v])

export default function Onboarding() {
  const me = useMe()
  const places = usePlaces()
  const dropoffs = knownPlaces(places.data)
  const [params, setParams] = useSearchParams()
  const step = Math.min(STEPS.length - 1, Math.max(0, Number(params.get('step') ?? 0) || 0))
  const next = safeNext(params.get('next'))
  const nav = useNavigate()
  const qc = useQueryClient()
  const reduced = useReducedMotion()
  const [dir, setDir] = useState(1)
  const [mapOpen, setMapOpen] = useState(false)
  const [pin, setPin] = useState<Place | null>(null)
  const [draft, setDraft] = useState<Draft | null>(() => loadDraft())
  const rests = useRestaurants({})

  useEffect(() => {
    document.title = 'Set up Ferry'
  }, [])

  // Seed the draft from the account once it loads.
  useEffect(() => {
    if (!draft && me.data) setDraft({ name: me.data.display_name, profile: me.data.profile, dayset: 'week' })
  }, [me.data, draft])

  useEffect(() => {
    if (!draft) return
    try {
      sessionStorage.setItem(DRAFT_KEY, JSON.stringify(draft))
    } catch {
      /* memory only */
    }
  }, [draft])

  const save = useMutation({
    mutationFn: () => api.saveProfile({ display_name: draft!.name.trim(), profile: draft!.profile }),
    onSuccess: (user) => {
      qc.setQueryData(keys.me, user)
      try {
        sessionStorage.removeItem(DRAFT_KEY)
      } catch {
        /* ignore */
      }
      pop(null, { label: 'You’re all set', sound: 'chime' })
      nav(next, { replace: true })
    },
  })

  const selectedMeals = useMemo(() => new Set(draft?.profile.usual_times.map((t) => t.label)), [draft])

  if (!draft) return <Loading label="Loading your answers" />

  const p = draft.profile
  const setP = (patch: Partial<Profile>) => setDraft({ ...draft, profile: { ...p, ...patch } })
  const go = (to: number) => {
    setDir(to > step ? 1 : -1)
    const n = new URLSearchParams(params)
    n.set('step', String(to))
    setParams(n, { replace: true })
  }
  const setMeals = (labels: Set<string>, dayset = draft.dayset) => {
    const days = DAYSETS.find((d) => d.key === dayset)!.days
    const usual_times: UsualTime[] = MEALS.filter((m) => labels.has(m.label)).map((m) => ({ label: m.label, days, start: m.start, end: m.end }))
    setDraft({ ...draft, dayset, profile: { ...p, usual_times } })
  }

  const canNext = STEPS[step] !== 'name' || draft.name.trim().length > 0
  const isLast = step === STEPS.length - 1

  const pickedLabel = p.dropoff_default?.label

  const content = (() => {
    switch (STEPS[step]) {
      case 'name':
        return (
          <div className={s.q}>
            <h1>What should other students call you?</h1>
            <p>This is the name your group sees on ferries and orders.</p>
            <Field label="Your name" value={draft.name} autoComplete="given-name" maxLength={40} onChange={(e) => setDraft({ ...draft, name: e.target.value })} autoFocus />
            <p className={s.agentNote}>
              <ChatCircleText size={20} weight="duotone" color="var(--sky-700)" aria-hidden style={{ flex: 'none' }} />
              The Ferry bot uses these answers when you message it on Telegram. Say “the usual” and it knows your drop-off, your diet and your tip.
            </p>
          </div>
        )
      case 'dropoff':
        return (
          <div className={s.q}>
            <h1>Where should food meet you?</h1>
            <p>Your default drop-off. You can change it on any order.</p>
            <div className={chipGroupClass} role="radiogroup" aria-label="Drop-off">
              {dropoffs.map((pl) => (
                <Chip key={pl.id} role="radio" selected={p.dropoff_default?.id === pl.id} onClick={() => setP({ dropoff_default: pl })}>
                  {pl.label}
                </Chip>
              ))}
              {p.dropoff_default && !p.dropoff_default.id && (
                <Chip role="radio" selected onClick={() => setMapOpen(true)}>
                  {p.dropoff_default.label}
                </Chip>
              )}
            </div>
            <GelButton variant="white" onClick={() => setMapOpen(true)} icon={<MapPin size={20} aria-hidden />}>
              Pick a spot on the map
            </GelButton>
            {pickedLabel && <p className="muted">Food will meet you at {pickedLabel}.</p>}
          </div>
        )
      case 'diet':
        return (
          <div className={s.q}>
            <h1>Anything we should know about what you eat?</h1>
            <div className={s.group}>
              <h2 id="diet-h">Diet</h2>
              <div className={chipGroupClass} role="group" aria-labelledby="diet-h">
                {DIETS.map((d) => (
                  <Chip key={d} selected={p.diet.includes(d.toLowerCase())} onClick={() => setP({ diet: toggle(p.diet, d.toLowerCase()) })}>
                    {d}
                  </Chip>
                ))}
              </div>
            </div>
            <div className={s.group}>
              <h2 id="avoid-h">Foods to avoid</h2>
              <div className={chipGroupClass} role="group" aria-labelledby="avoid-h">
                {AVOIDS.map((d) => (
                  <Chip key={d} selected={p.avoid.includes(d.toLowerCase())} onClick={() => setP({ avoid: toggle(p.avoid, d.toLowerCase()) })}>
                    {d}
                  </Chip>
                ))}
              </div>
            </div>
          </div>
        )
      case 'favorites':
        return (
          <div className={s.q}>
            <h1>What do you usually crave?</h1>
            <div className={s.group}>
              <h2 id="cui-h">Cuisines</h2>
              <div className={chipGroupClass} role="group" aria-labelledby="cui-h">
                {CUISINES.map((c) => (
                  <Chip key={c} selected={p.favorite_cuisines.includes(c.toLowerCase())} onClick={() => setP({ favorite_cuisines: toggle(p.favorite_cuisines, c.toLowerCase()) })}>
                    {c}
                  </Chip>
                ))}
              </div>
            </div>
            <div className={s.group}>
              <h2 id="rst-h">Places you like</h2>
              <div className={chipGroupClass} role="group" aria-labelledby="rst-h">
                {rests.isPending
                  ? 'Loading restaurants'
                  : rests.data?.map((r) => (
                      <Chip key={r.id} selected={p.favorite_restaurant_ids.includes(r.id)} onClick={() => setP({ favorite_restaurant_ids: toggle(p.favorite_restaurant_ids, r.id) })}>
                        {r.name}
                      </Chip>
                    ))}
              </div>
            </div>
          </div>
        )
      case 'times':
        return (
          <div className={s.q}>
            <h1>When do you usually eat?</h1>
            <p>Ferry tells you when students head out around these times.</p>
            <div className={s.group}>
              <h2 id="meal-h">Meals</h2>
              <div className={chipGroupClass} role="group" aria-labelledby="meal-h">
                {MEALS.map((m) => (
                  <Chip key={m.label} selected={selectedMeals.has(m.label)} onClick={() => {
                    const n = new Set(selectedMeals)
                    if (n.has(m.label)) n.delete(m.label)
                    else n.add(m.label)
                    setMeals(n)
                  }}>
                    {m.label}
                  </Chip>
                ))}
              </div>
            </div>
            <div className={s.group}>
              <h2 id="days-h">Days</h2>
              <div className={chipGroupClass} role="radiogroup" aria-labelledby="days-h">
                {DAYSETS.map((d) => (
                  <Chip key={d.key} role="radio" selected={draft.dayset === d.key} onClick={() => setMeals(selectedMeals, d.key)}>
                    {d.label}
                  </Chip>
                ))}
              </div>
            </div>
          </div>
        )
      case 'ferrying':
        return (
          <div className={s.q}>
            <h1>How do you walk?</h1>
            <div className={chipGroupClass} role="radiogroup" aria-label="Walking pace">
              {PACES.map((pc) => (
                <Chip key={pc.key} role="radio" selected={p.walking_pace === pc.key} onClick={() => setP({ walking_pace: pc.key })}>
                  {pc.label}
                </Chip>
              ))}
            </div>
            <p className="muted">{PACES.find((x) => x.key === p.walking_pace)?.help}. We use it for ETAs when you ferry.</p>
            <Panel pad="tight">
              <SwitchRow label="I’m open to ferrying for others" help="When you head out, Ferry opens your trip to your group." checked={p.runner_opt_in} onChange={(v) => setP({ runner_opt_in: v })} />
              <SwitchRow label="Share my location while I’m ferrying" help="Only during an active run, so customers can see you coming." checked={p.share_location_when_running} onChange={(v) => setP({ share_location_when_running: v })} />
            </Panel>
          </div>
        )
      case 'reach': {
        const reach = p.max_dropoff_distance_m ?? 500
        const home = p.dropoff_default ?? dropoffs[0]
        return (
          <div className={s.q}>
            <h1>How far would you walk to drop food off?</h1>
            <p className="muted">When you bring food back for others, you hand it over within this distance of where you’re heading back to. Ferry fills it in each time you go somewhere, and you can change it then.</p>
            <div className={chipGroupClass} role="radiogroup" aria-label="Drop-off distance">
              {REACH.map((m) => (
                <Chip key={m} role="radio" selected={reach === m} onClick={() => setP({ max_dropoff_distance_m: m })}>
                  {m < 1000 ? `${m} m` : `${m / 1000} km`}, {walkMinutes(m)} min
                </Chip>
              ))}
            </div>
            <div className={s.reachMap}>
              <LazyMap label={`Map of ${home.label} with a ${reach} m circle`} center={home.location} zoom={reach > 600 ? 14.2 : 15.2} interactive={false} zone={{ center: home.location, radius_m: reach }} markers={[{ id: 'home', at: home.location, kind: 'dropoff', label: home.label }]} />
            </div>
          </div>
        )
      }
      case 'finish':
        return (
          <div className={s.q}>
            <h1>Tip and notifications</h1>
            <div className={s.group}>
              <Field
                type="range"
                label="Default tip for your captain"
                min={0}
                max={500}
                step={50}
                value={p.default_tip_cents}
                onChange={(e) => setP({ default_tip_cents: Number(e.target.value) })}
                aria-valuetext={money(p.default_tip_cents)}
              />
              <p className={`${s.tipValue} num`} aria-hidden>
                {money(p.default_tip_cents)}
              </p>
              <p className="muted">All of it goes to the captain who walks your food. You can change it on each order.</p>
            </div>
            <Panel pad="tight">
              <SwitchRow
                label="App notifications"
                help={notifications.supported ? 'When a ferry opens, your food moves, or someone invites you.' : 'This browser doesn’t support notifications. You’ll still see them in the app.'}
                checked={p.notify_push}
                onChange={(v) => {
                  setP({ notify_push: v })
                  if (v) void notifications.request()
                }}
              />
              <SwitchRow label="Messages from Ferry" help="Updates in your Telegram groups and chats." checked={p.notify_messages} onChange={(v) => setP({ notify_messages: v })} />
            </Panel>
            <p className={s.agentNote}>
              <ChatCircleText size={20} weight="duotone" color="var(--sky-700)" aria-hidden style={{ flex: 'none' }} />
              That’s it. The Ferry agent reads these answers whenever you text it, and you can edit them in Settings.
            </p>
            {save.isError && (
              <p className={s.error} role="alert">
                {save.error instanceof ApiError ? save.error.message : 'Your answers didn’t save. Try again.'}
              </p>
            )}
          </div>
        )
    }
  })()

  return (
    <MotionConfig reducedMotion={reduced ? 'always' : 'never'}>
      <div className={s.page}>
        <header className={s.head}>
          <WaterGlass progress={(step + (isLast ? 1 : 0.5)) / STEPS.length} size={44} />
          <div className={s.headText} aria-live="polite">
            <strong>Set up Ferry</strong>
            <span>
              Step {step + 1} of {STEPS.length}
            </span>
          </div>
        </header>
        <main className={s.body} id="main">
          <AnimatePresence mode="wait" initial={false} custom={dir}>
            <motion.div
              key={step}
              custom={dir}
              initial={{ x: 24 * dir, opacity: 0 }}
              animate={{ x: 0, opacity: 1 }}
              exit={{ x: -24 * dir, opacity: 0 }}
              transition={{ type: 'spring', stiffness: 520, damping: 34 }}
            >
              {content}
            </motion.div>
          </AnimatePresence>
        </main>
        <footer className={`${s.foot} ${glassClass}`}>
          <div className={s.footInner}>
            {step > 0 ? (
              <GelButton variant="white" onClick={() => go(step - 1)} icon={<ArrowLeft size={18} aria-hidden />}>
                Back
              </GelButton>
            ) : (
              <span />
            )}
            {isLast ? (
              <GelButton busy={save.isPending} onClick={() => save.mutate()}>
                Save and start
              </GelButton>
            ) : (
              <GelButton disabled={!canNext} onClick={() => go(step + 1)}>
                Next
              </GelButton>
            )}
          </div>
        </footer>
        <Sheet
          open={mapOpen}
          onClose={() => setMapOpen(false)}
          title="Pick your drop-off"
          subtitle="Tap the map where food should meet you."
          footer={
            <GelButton
              block
              disabled={!pin}
              onClick={() => {
                if (pin) setP({ dropoff_default: pin })
                setMapOpen(false)
              }}
            >
              Use this spot
            </GelButton>
          }
        >
          <div className={s.mapBox}>
            {mapOpen && (
              <LazyMap
                label="Map of campus. Tap to choose your drop-off."
                center={p.dropoff_default?.location ?? CAMPUS_CENTER}
                zoom={15}
                onPick={(at) => setPin({ label: 'Pinned spot', location: at })}
                markers={[
                  ...dropoffs.map((d) => ({ id: d.id!, at: d.location, kind: 'dropoff' as const, label: d.label })),
                  ...(pin ? [{ id: 'pin', at: pin.location, kind: 'pin' as const, label: 'Your spot' }] : []),
                ]}
              />
            )}
          </div>
        </Sheet>
      </div>
    </MotionConfig>
  )
}
