import { ChatCircleText, Check, TelegramLogo, X } from '@phosphor-icons/react'
import { useEffect, useRef } from 'react'
import { useParams } from 'react-router-dom'
import { useCircles, useInvites, useMe, useRespondInvite } from '../api/hooks'
import { ApiError } from '../api/client'
import { dayTime } from '../lib/format'
import { composeLink, composeQrPayload, openExternal, platform } from '../platform'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar } from '../ui/Bubble'
import { GelAnchor, GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { QR } from '../ui/QR'
import { Empty, ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Invite.module.css'

const word = { pending: 'Hasn’t answered', accepted: 'Going', declined: 'Can’t make it' }

export default function InviteScreen() {
  const { inviteId = '' } = useParams()
  const invites = useInvites()
  const circles = useCircles()
  const me = useMe()
  const respond = useRespondInvite()
  const inv = invites.data?.find((i) => i.id === inviteId)
  const mine = inv?.responses.find((r) => r.user.id === me.data?.id)
  const acceptBtn = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (inv) document.title = `${inv.from.display_name} invited you`
  }, [inv])

  const body = (() => {
    if (invites.isPending) return <Loading />
    if (invites.isError) return <ErrorState error={invites.error} onRetry={() => invites.refetch()} />
    if (!inv) return <Empty title="This invite isn’t here">It may have expired, or it was sent to someone else.</Empty>
    const circle = circles.data?.find((x) => x.id === inv.circle_id)
    const compose = inv.thread.compose
    const openUrl = inv.thread.open_url ?? null
    const link = compose ? composeLink(compose) : null
    return (
      <div className="stack">
        <Panel className={s.card}>
          <div className={s.from}>
            <Avatar user={inv.from} size={56} />
            <div>
              <h1>{inv.from.display_name} wants to meet again</h1>
              {circle && (
                <p className="muted">
                  <span aria-hidden>{circle.emoji}</span> {circle.name}
                </p>
              )}
            </div>
          </div>
          <p className={s.message}>“{inv.message}”</p>
          {inv.proposed_time && (
            <p>
              <strong>When:</strong> {dayTime(inv.proposed_time)}
            </p>
          )}
          <ul className={s.responses} aria-label="Who’s going">
            {inv.responses.map((r) => (
              <li key={r.user.id}>
                <Avatar user={r.user} size={32} label={false} />
                <span>{r.user.id === me.data?.id ? 'You' : r.user.display_name}</span>
                <span className={`${c.pill} ${r.status === 'accepted' ? '' : c.pillMuted}`}>{word[r.status]}</span>
              </li>
            ))}
          </ul>
          {mine?.status === 'pending' && inv.status !== 'expired' && (
            <div className={s.actions}>
              <GelButton
                ref={acceptBtn}
                size="lg"
                busy={respond.isPending && respond.variables?.accept}
                icon={<Check size={20} weight="bold" aria-hidden />}
                onClick={() =>
                  respond.mutate(
                    { id: inv.id, accept: true },
                    { onSuccess: () => pop(acceptBtn.current, { label: 'You’re in', sound: 'chime', size: 64 }) },
                  )
                }
              >
                I’m in
              </GelButton>
              <GelButton variant="white" size="lg" busy={respond.isPending && respond.variables?.accept === false} icon={<X size={20} aria-hidden />} onClick={() => respond.mutate({ id: inv.id, accept: false })}>
                Can’t make it
              </GelButton>
            </div>
          )}
          {respond.isError && (
            <p className={c.error} role="alert">
              {respond.error instanceof ApiError ? respond.error.message : 'That didn’t go through. Try again.'}
            </p>
          )}
        </Panel>

        {mine?.status === 'accepted' && (
          <Panel className={s.thread} aria-live="polite">
            {inv.thread.state === 'ready' && openUrl ? (
              <>
                <h2>Your group chat is ready</h2>
                <p className="muted">Ferry made a Telegram group with everyone who said yes.</p>
                <GelAnchor
                  href={openUrl}
                  size="lg"
                  target="_blank"
                  rel="noopener"
                  icon={<TelegramLogo size={22} weight="fill" aria-hidden />}
                  onClick={(e) => {
                    if (platform.isNative) {
                      e.preventDefault()
                      openExternal(openUrl)
                    }
                  }}
                >
                  Open in Telegram
                </GelAnchor>
                {!platform.isPhone && (
                  <div className={s.qr}>
                    <QR value={openUrl} label="QR code that opens the group in Telegram" />
                    <p className="muted">Or scan with your phone to open the group there.</p>
                  </div>
                )}
              </>
            ) : inv.thread.state === 'ready' && compose ? (
              <>
                <h2>Your group chat is ready</h2>
                <p className="muted">Ferry put everyone who said yes into one thread.</p>
                {link ? (
                  <GelAnchor href={link} size="lg" icon={<ChatCircleText size={22} weight="fill" aria-hidden />}>
                    Open Messages
                  </GelAnchor>
                ) : (
                  <div className={s.qr}>
                    <QR value={composeQrPayload(compose)} label="QR code that opens the group text on your phone" />
                    <p className="muted">Scan with your phone to open the group text.</p>
                  </div>
                )}
              </>
            ) : inv.thread.state === 'creating' ? (
              <>
                <h2>Starting your group chat</h2>
                <p className="muted">Ferry is adding everyone who said yes. This takes a few seconds.</p>
              </>
            ) : (
              <>
                <h2>You’re going</h2>
                <p className="muted">Ferry starts a Telegram group once others say yes.</p>
              </>
            )}
          </Panel>
        )}
      </div>
    )
  })()

  return (
    <>
      <TopBar title="Invite" back="/circles" />
      <Page>{body}</Page>
    </>
  )
}
