import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { upsertRun, useCircles, useMe, usePlaces, useRestaurants } from '../api/hooks'
import { clock } from '../lib/format'
import { knownPlaces } from '../lib/places'
import { useNow } from '../lib/time'
import { rememberRun } from '../state/myRuns'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar } from '../ui/Bubble'
import { Chip, chipGroupClass } from '../ui/Chip'
import { Field } from '../ui/Field'
import { GelButton } from '../ui/GelButton'
import { LocationPicker, type PlaceZone } from '../ui/LocationPicker'
import { pop } from '../ui/pops'
import { ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './NewRun.module.css'

const LEAVING = [10, 15, 20, 30]
const WINDOWS = [5, 10, 15]
const BACK = [20, 30, 45, 60]

// The server's limits (runs/actions.rs): leaving at most 24 h ahead, orders open
// 1 to 120 min, back at most 24 h after leaving.
const LIMITS = { leaving: [0, 1440], win: [1, 120], back: [1, 1440] } as const

// A typed number of minutes, or null when it's empty or out of range.
const typedMinutes = (text: string, [min, max]: readonly [number, number]) => {
  if (!/^\d+$/.test(text)) return null
  const n = Number(text)
  return n >= min && n <= max ? n : null
}

// "Or type minutes": the buttons stay; a valid number here wins while it's typed.
function MinutesField({ label, text, onText, limits }: { label: string; text: string; onText: (t: string) => void; limits: readonly [number, number] }) {
  const bad = text !== '' && typedMinutes(text, limits) === null
  return (
    <div className={s.typed}>
      <Field
        label={label}
        type="text"
        inputMode="numeric"
        pattern="[0-9]*"
        maxLength={4}
        placeholder="Minutes"
        autoComplete="off"
        value={text}
        onChange={(e) => onText(e.target.value.replace(/\D/g, ''))}
        error={bad ? `Type ${limits[0]} to ${limits[1]} minutes.` : undefined}
      />
    </div>
  )
}

// Going somewhere: where, when you leave, when you're back and where to, and
// how far you'll walk to hand food over. Customers see all of it on the ferry.
export default function NewRun() {
  const rests = useRestaurants({ open_now: true })
  const me = useMe()
  const places = usePlaces()
  const circles = useCircles()
  const nav = useNavigate()
  const qc = useQueryClient()
  const now = useNow(30_000)
  const [params] = useSearchParams()
  const [picked, setRid] = useState<string | null>(null)
  // A "Someone near your route wants …" notice links here with ?restaurant=<id>: preselect it.
  const asked = params.get('restaurant')
  const rid = picked ?? (rests.data?.some((r) => r.id === asked) ? asked : null)
  const [cid, setCid] = useState<string | null>(null)
  const [leavingPick, setLeavingPick] = useState(15)
  const [winPick, setWinPick] = useState(10)
  const [backPick, setBackPick] = useState(30)
  const [leavingText, setLeavingText] = useState('')
  const [winText, setWinText] = useState('')
  const [backText, setBackText] = useState('')
  const leavingTyped = typedMinutes(leavingText, LIMITS.leaving)
  const winTyped = typedMinutes(winText, LIMITS.win)
  const backTyped = typedMinutes(backText, LIMITS.back)
  const leaving = leavingTyped ?? leavingPick
  const win = winTyped ?? winPick
  const back = backTyped ?? backPick
  // A number typed but out of range blocks opening until it's fixed or cleared.
  const typedBad = [leavingText && leavingTyped === null, winText && winTyped === null, backText && backTyped === null].some(Boolean)
  const [ret, setRet] = useState<PlaceZone | null>(null)
  const profile = me.data?.profile
  const saved = knownPlaces(places.data)
  // The trip opens to one of your circles: the one you pick, else your first.
  const circle = circles.data?.find((x) => x.id === cid) ?? circles.data?.[0]

  // Default the way back to your usual drop-off and your usual walking distance.
  const retValue: PlaceZone = ret ?? { place: profile?.dropoff_default ?? saved[0], radius_m: profile?.max_dropoff_distance_m ?? 400 }
  const leaveAt = now + leaving * 60e3
  const closesAt = leaveAt + win * 60e3
  const backAt = leaveAt + back * 60e3
  const restaurant = rests.data?.find((r) => r.id === rid)

  const create = useMutation({
    mutationFn: () => {
      const submittedLeaveAt = Date.now() + leaving * 60e3
      return api.createRun({
        restaurant_id: rid!,
        circle_id: circle!.id,
        leaving_at: new Date(submittedLeaveAt).toISOString(),
        window_minutes: win,
        returning_at: new Date(submittedLeaveAt + back * 60e3).toISOString(),
        return_place: retValue.place,
        dropoff_radius_m: retValue.radius_m,
      })
    },
    onSuccess: (run) => {
      upsertRun(qc, run)
      rememberRun(run.id)
      pop(null, { label: 'Ferry open', sound: 'chime' })
      nav(`/run/${run.id}`, { replace: true })
    },
  })

  useEffect(() => {
    document.title = 'I’m going somewhere'
  }, [])

  return (
    <>
      <TopBar title="I’m going somewhere" back="/home" />
      <Page>
        <div className="stack">
          <p className="muted">Open your trip to your group. Ferry collects their orders and holds their money; you carry the food back on your way.</p>

          <Panel className="stack">
            <h2 id="where-h">Where are you going?</h2>
            {rests.isPending ? (
              <Loading rows={1} />
            ) : (
              <div className={chipGroupClass} role="radiogroup" aria-labelledby="where-h">
                {rests.data?.map((r) => (
                  <Chip key={r.id} role="radio" selected={rid === r.id} onClick={() => setRid(r.id)}>
                    {r.name}
                  </Chip>
                ))}
              </div>
            )}
          </Panel>

          <Panel className="stack">
            <h2 id="leave-h">When are you leaving?</h2>
            <div className={chipGroupClass} role="radiogroup" aria-labelledby="leave-h">
              {LEAVING.map((m) => (
                <Chip key={m} role="radio" selected={leaving === m} onClick={() => { setLeavingPick(m); setLeavingText('') }}>
                  In {m} min
                </Chip>
              ))}
            </div>
            <MinutesField label="Or type minutes from now" text={leavingText} onText={setLeavingText} limits={LIMITS.leaving} />
            <p className={s.time}>
              Leaving at <strong className="num">{clock(new Date(leaveAt).toISOString())}</strong>
            </p>
            <h3 id="win-h">Keep orders open after leaving</h3>
            <div className={chipGroupClass} role="radiogroup" aria-labelledby="win-h">
              {WINDOWS.map((m) => (
                <Chip key={m} role="radio" selected={win === m} onClick={() => { setWinPick(m); setWinText('') }}>
                  {m} min
                </Chip>
              ))}
            </div>
            <MinutesField label="Or type minutes after leaving" text={winText} onText={setWinText} limits={LIMITS.win} />
            <p className={s.time}>
              Orders close at <strong className="num">{clock(new Date(closesAt).toISOString())}</strong>
            </p>
          </Panel>

          <Panel className="stack">
            <h2 id="back-h">When will you be back?</h2>
            <div className={chipGroupClass} role="radiogroup" aria-labelledby="back-h">
              {BACK.map((m) => (
                <Chip key={m} role="radio" selected={back === m} onClick={() => { setBackPick(m); setBackText('') }}>
                  {m} min after
                </Chip>
              ))}
            </div>
            <MinutesField label="Or type minutes after leaving" text={backText} onText={setBackText} limits={LIMITS.back} />
            <p className={s.time}>
              Back around <strong className="num">{clock(new Date(backAt).toISOString())}</strong>
            </p>
          </Panel>

          <Panel className="stack">
            <h2>Where are you heading back to?</h2>
            <p className="muted">Your group near there can get food from you. Set how far you’ll walk from there to hand it over.</p>
            <LocationPicker
              value={retValue}
              onChange={setRet}
              heading="Heading back to"
              zoneLabel="Drops off within"
              sheetTitle="Where are you heading back to?"
              sheetSubtitle="Drag the map to your spot, then set how far you’ll walk to drop food off."
              saved={saved}
              minRadius={100}
              maxRadius={1500}
            />
          </Panel>

          <Panel className="stack">
            <h2 id="who-h">Who can order?</h2>
            {circles.isPending ? (
              <Loading rows={1} />
            ) : circles.isError && !circles.data ? (
              <ErrorState error={circles.error} onRetry={() => circles.refetch()} />
            ) : !circles.data?.length ? (
              <p className="muted">You’re not in a group yet. Add @Ferrygo_bot to a Telegram group chat; that group becomes your circle, and your trips open to it.</p>
            ) : circles.data.length === 1 ? (
              <p className="muted">
                Everyone in <strong className={s.circle}><span aria-hidden>{circle!.emoji}</span> {circle!.name}</strong> can see your trip and order.
              </p>
            ) : (
              <>
                <p className="muted">Pick the group your trip opens to. Everyone in it can see your trip and order.</p>
                <div className={chipGroupClass} role="radiogroup" aria-labelledby="who-h">
                  {circles.data.map((ci) => (
                    <Chip key={ci.id} role="radio" selected={circle?.id === ci.id} onClick={() => setCid(ci.id)}>
                      <span aria-hidden>{ci.emoji}</span> {ci.name}
                    </Chip>
                  ))}
                </div>
              </>
            )}
          </Panel>

          {me.data && (
            <section className={s.preview} aria-labelledby="preview-h">
              <h2 id="preview-h">What your group will see</h2>
              <div className={s.previewCard}>
                <Avatar user={me.data} size={44} label={false} />
                <p>
                  <strong>{me.data.display_name || 'You'}</strong> {restaurant ? `is going to ${restaurant.name}.` : 'is going out for food.'} Leaves at {clock(new Date(leaveAt).toISOString())}, takes orders until {clock(new Date(closesAt).toISOString())}, back around {clock(new Date(backAt).toISOString())} to {retValue.place.label}. Drops off within {retValue.radius_m} m.
                </p>
              </div>
            </section>
          )}

          {create.isError && (
            <p className={c.error} role="alert">
              {create.error instanceof ApiError ? create.error.message : 'The ferry didn’t open. Try again.'}
            </p>
          )}
          <GelButton size="lg" busy={create.isPending} disabled={!rid || !circle || typedBad} onClick={() => create.mutate()}>
            Open the ferry
          </GelButton>
        </div>
      </Page>
    </>
  )
}
