import { UsersThree } from '@phosphor-icons/react'
import { useEffect } from 'react'
import { Link } from 'react-router-dom'
import { useCircles, useInvites, useMe } from '../api/hooks'
import { ago, dayTime, grams, plural } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar, AvatarStack, Bubble } from '../ui/Bubble'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'

export default function Circles() {
  const circles = useCircles()
  const invites = useInvites()
  const me = useMe()
  const pending = invites.data?.filter((i) => i.status === 'pending' && i.responses.find((r) => r.user.id === me.data?.id)?.status === 'pending') ?? []

  useEffect(() => {
    document.title = 'Your circles on Ferry'
  }, [])

  return (
    <>
      <TopBar title="Circles" />
      <Page>
        {pending.length > 0 && (
          <section className={c.section} aria-labelledby="inv-h">
            <div className={c.sectionHead}>
              <h2 id="inv-h">Invites</h2>
            </div>
            <ul className={c.list}>
              {pending.map((i) => (
                <Panel as="li" pad="flush" key={i.id}>
                  <Link to={`/i/${i.id}`} className={c.row}>
                    <Avatar user={i.from} size={44} />
                    <span className={c.rowMain}>
                      <h3>{i.from.display_name} wants to meet again</h3>
                      <p>
                        {i.message}
                        {i.proposed_time ? ` ${dayTime(i.proposed_time)}.` : ''}
                      </p>
                    </span>
                    <span className={c.rowAside}>
                      <span className={c.pill}>Reply</span>
                    </span>
                  </Link>
                </Panel>
              ))}
            </ul>
          </section>
        )}
        <section className={c.section} aria-labelledby="cir-h">
          <div className={c.sectionHead}>
            <h2 id="cir-h">People you ferry with</h2>
          </div>
          {circles.isPending ? (
            <div className="stack" aria-hidden>
              <Skeleton h={96} />
              <Skeleton h={96} />
            </div>
          ) : circles.isError ? (
            <Panel>
              <ErrorState error={circles.error} onRetry={() => circles.refetch()} />
            </Panel>
          ) : circles.data.length === 0 ? (
            <Panel>
              <Empty icon={<UsersThree size={28} color="var(--sky-700)" aria-hidden />} title="No circles yet">
                Circles form from the group chats Ferry is in and the people you ferry with.
              </Empty>
            </Panel>
          ) : (
            <ul className={c.list}>
              {circles.data.map((ci) => (
                <Panel as="li" pad="flush" key={ci.id}>
                  <Link to={`/circles/${ci.id}`} className={c.row}>
                    <Bubble size={52} clear seed={ci.id}>
                      <span aria-hidden style={{ fontSize: 26 }}>
                        {ci.emoji}
                      </span>
                    </Bubble>
                    <span className={c.rowMain}>
                      <h3>{ci.name}</h3>
                      <p>
                        {plural(ci.members.length, 'person', 'people')}. {ci.last_met_at ? `Last met ${ago(ci.last_met_at)}.` : 'Haven’t met up yet.'}
                      </p>
                      <AvatarStack users={ci.members} size={26} />
                    </span>
                    <span className={c.rowAside}>
                      <span className="num">{plural(ci.stats.runs, 'run')}</span>
                      <span className="num co2" style={{ fontWeight: 600 }}>
                        {grams(ci.stats.co2_saved_g)} CO2
                      </span>
                    </span>
                  </Link>
                </Panel>
              ))}
            </ul>
          )}
        </section>
      </Page>
    </>
  )
}
