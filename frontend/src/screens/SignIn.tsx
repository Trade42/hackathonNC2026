import { TelegramLogo } from '@phosphor-icons/react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { Link, Navigate, useNavigate, useSearchParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, useConfig, useToken } from '../api/hooks'
import type { AuthStart, Persona, User } from '../api/types'
import { env } from '../config/env'
import { safeNext } from '../lib/next'
import { openExternal, platform } from '../platform'
import { Scenery } from '../scene/Scenery'
import { Logo } from '../shell/Logo'
import { session } from '../state/session'
import { Avatar } from '../ui/Bubble'
import { GelAnchor, GelButton } from '../ui/GelButton'
import { QR } from '../ui/QR'
import { Reveal } from '../ui/Reveal'
import { Panel } from '../ui/Surface'
import s from './SignIn.module.css'

// Contract v1.1 demo personas (fixed IDs and names).
const personas: { persona: Persona; name: string; color: string; role: string }[] = [
  { persona: 'judge', name: 'Jordan', color: '#1E8FD6', role: 'Judges start here. Orders from Maya’s ferry.' },
  { persona: 'maya', name: 'Maya', color: '#E0683C', role: 'The captain. Closes the window and walks the food.' },
  { persona: 'dana', name: 'Dana', color: '#8A5CD0', role: 'Friend on the 4th floor. Sends the meetup invite.' },
  { persona: 'leo', name: 'Leo', color: '#2E9A3B', role: 'Runs cookie trips from Insomnia.' },
]

// The open challenge survives a trip to Telegram and back, even if iOS closes
// the Home Screen web app while it is in the background.
const CHALLENGE_KEY = 'ferry.tgChallenge'
const loadChallenge = (): AuthStart | null => {
  try {
    const c = JSON.parse(localStorage.getItem(CHALLENGE_KEY) ?? 'null') as AuthStart | null
    return c && /^\d{4}$/.test(c.confirm_code) && Date.parse(c.expires_at) > Date.now() ? c : null
  } catch {
    return null
  }
}
const saveChallenge = (c: AuthStart | null) => {
  try {
    if (c) localStorage.setItem(CHALLENGE_KEY, JSON.stringify(c))
    else localStorage.removeItem(CHALLENGE_KEY)
  } catch {
    /* memory only */
  }
}

export default function SignIn() {
  const token = useToken()
  const [params] = useSearchParams()
  const next = safeNext(params.get('next'))
  const nav = useNavigate()
  const qc = useQueryClient()
  const { data: cfg } = useConfig()
  const [challenge, setChallengeState] = useState<AuthStart | null>(loadChallenge)
  const setChallenge = (c: AuthStart | null) => {
    saveChallenge(c)
    setChallengeState(c)
  }
  const isPhone = platform.isPhone

  useEffect(() => {
    document.title = 'Sign in to Ferry'
  }, [])

  const finish = (tok: string, user: User) => {
    setChallenge(null)
    session.set(tok)
    qc.clear()
    qc.setQueryData(keys.me, user)
    nav(user.onboarding_complete ? next : `/welcome?next=${encodeURIComponent(next)}`, { replace: true })
  }

  // Open the challenge up front, so "Continue with Telegram" is a plain link
  // the tap opens straight away (no popup blocker after an await).
  const start = useMutation({ mutationFn: api.telegramStart, onSuccess: setChallenge })
  const autoStarted = useRef(false)
  useEffect(() => {
    if (autoStarted.current || token || challenge) return
    autoStarted.current = true
    start.mutate()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token])

  const poll = useQuery({
    queryKey: ['auth', challenge?.challenge_id],
    queryFn: () => api.telegramPoll(challenge!.challenge_id),
    enabled: !!challenge && !token,
    refetchInterval: (q) => (q.state.data?.status === 'pending' || !q.state.data ? 2000 : false),
    refetchIntervalInBackground: true,
    gcTime: 0,
  })

  useEffect(() => {
    const d = poll.data
    if (d?.status === 'verified' && d.token && d.user && !session.token) finish(d.token, d.user)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [poll.data])

  const demo = useMutation({
    mutationFn: api.authDemo,
    onSuccess: (r) => {
      // A pending Telegram challenge must not sign someone else in afterwards.
      setChallenge(null)
      finish(r.token, r.user)
    },
  })

  if (token && !challenge) return <Navigate to={next} replace />

  const expired = poll.data?.status === 'expired' || (poll.data?.status === 'verified' && !poll.data.token) || poll.error instanceof ApiError
  const restart = () => {
    setChallenge(null)
    start.mutate()
  }

  return (
    <div className={s.page}>
      <Scenery variant="compact" />
      <div className={s.top}>
        <Link to="/" aria-label="Ferry home">
          <Logo size={28} />
        </Link>
      </div>
      <main className={s.grid} id="main">
        <Reveal>
          <Panel className={s.card}>
            <h1 className={s.title}>Sign in with Telegram</h1>
            <p className={s.lead}>
              {isPhone
                ? 'Tap the button, then tap Start and Confirm in the Ferry chat. You’re signed in when you come back. No password.'
                : 'Scan the QR code with your phone, then tap Start and Confirm in the Ferry chat.'}
            </p>

            {!challenge && start.isPending && <div className={s.qrSkeleton} aria-hidden />}

            {challenge && !expired && (
              <>
                {!isPhone && (
                  <div className={s.qrRow}>
                    <QR value={challenge.bot_url} label="QR code that opens the Ferry bot in Telegram" />
                    <ol className={s.steps}>
                      <li>
                        <span>Point your phone’s camera at the code.</span>
                      </li>
                      <li>
                        <span>
                          Tap <strong>Start</strong> in the Ferry chat.
                        </span>
                      </li>
                      <li>
                        <span>Check the matching number, then tap Confirm.</span>
                      </li>
                    </ol>
                  </div>
                )}
                <GelAnchor
                  href={challenge.bot_url}
                  size="lg"
                  variant={isPhone ? 'sky' : 'white'}
                  target="_blank"
                  rel="noopener"
                  icon={<TelegramLogo size={22} weight="fill" aria-hidden />}
                  onClick={(e) => {
                    // Mock mode has no real bot: tapping stands in for Start.
                    if (env.isMock) {
                      e.preventDefault()
                      void import('../mocks/db').then((m) => m.openChallenge(challenge.challenge_id))
                      return
                    }
                    if (platform.isNative) {
                      e.preventDefault()
                      openExternal(challenge.bot_url)
                    }
                  }}
                >
                  {isPhone ? 'Continue with Telegram' : 'Open Telegram here'}
                </GelAnchor>
                <p className={s.confirm}>
                  Telegram will show <strong className="num">{challenge.confirm_code}</strong>. Confirm only if it matches.
                </p>
                <p className={s.waiting} role="status" aria-live="polite">
                  <span className={s.dots} aria-hidden>
                    <span />
                    <span />
                    <span />
                  </span>
                  Waiting for you to tap Start and Confirm in Telegram
                </p>
              </>
            )}

            {expired && (
              <div className="stack">
                <p className={s.error} role="alert">
                  That sign-in link expired or could not finish. Get a new one and tap Start within 10 minutes.
                </p>
                <GelButton onClick={restart}>Get a new link</GelButton>
              </div>
            )}
            {start.isError && (
              <p className={s.error} role="alert">
                {start.error instanceof ApiError ? start.error.message : 'We couldn’t start sign-in.'}{' '}
                <button type="button" className={s.linkBtn} onClick={() => start.mutate()}>
                  Try again
                </button>
              </p>
            )}
          </Panel>
        </Reveal>

        {cfg?.demo_mode && (
          <Reveal delay={0.08}>
            <Panel className={s.card} aria-labelledby="demo-h">
              <div>
                <h2 id="demo-h">Demo accounts</h2>
                <p className="muted">Sign in as someone from the Hinton James 4th floor group.</p>
              </div>
              <div className={s.personas}>
                {personas.map(({ persona, name, color, role }) => {
                  const u = { display_name: name, bubble_color: color, avatar_seed: persona }
                  return (
                    <button
                      key={persona}
                      type="button"
                      className={`${s.persona} ${persona === 'judge' ? s.personaPrimary : ''}`}
                      onClick={() => demo.mutate(persona)}
                      disabled={demo.isPending}
                      aria-busy={demo.isPending && demo.variables === persona}
                    >
                      <Avatar user={u} size={40} label={false} />
                      <span>
                        <strong>Continue as {u.display_name}</strong>
                        <small>{role}</small>
                      </span>
                    </button>
                  )
                })}
              </div>
              {demo.isError && (
                <p className={s.error} role="alert">
                  {demo.error instanceof ApiError ? demo.error.message : 'That didn’t work. Try again.'}
                </p>
              )}
            </Panel>
          </Reveal>
        )}
      </main>
    </div>
  )
}
