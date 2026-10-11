import { Car, Leaf, PersonSimpleWalk } from '@phosphor-icons/react'
import { useEffect, useState } from 'react'
import { useCircles, useImpact } from '../api/hooks'
import type { ImpactScope } from '../api/types'
import { grams } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { Bubble } from '../ui/Bubble'
import { Chip, chipGroupClass } from '../ui/Chip'
import { ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import { WeeklyBubbles } from '../ui/WeeklyBubbles'
import c from './common.module.css'
import s from './Impact.module.css'

const TABS: { key: ImpactScope; label: string }[] = [
  { key: 'me', label: 'Me' },
  { key: 'circle', label: 'My circles' },
  { key: 'campus', label: 'Campus' },
]

export default function ImpactScreen() {
  const [scope, setScope] = useState<ImpactScope>(() => {
    try {
      return (sessionStorage.getItem('ferry.impactTab') as ImpactScope) || 'me'
    } catch {
      return 'me'
    }
  })
  const circles = useCircles()
  const [circleId, setCircleId] = useState<string | undefined>()
  const cid = scope === 'circle' ? (circleId ?? circles.data?.[0]?.id) : undefined
  const impact = useImpact(scope, cid)

  useEffect(() => {
    document.title = 'Your impact on Ferry'
    try {
      sessionStorage.setItem('ferry.impactTab', scope)
    } catch {
      /* ignore */
    }
  }, [scope])

  const d = impact.data
  const who = scope === 'me' ? 'You' : scope === 'campus' ? 'Campus' : (circles.data?.find((x) => x.id === cid)?.name ?? 'Your circle')

  return (
    <>
      <TopBar title="Impact" />
      <Page>
        <div className={chipGroupClass} role="group" aria-label="Whose impact">
          {TABS.map((t) => (
            <Chip key={t.key} selected={scope === t.key} onClick={() => setScope(t.key)} aria-controls="impact-panel">
              {t.label}
            </Chip>
          ))}
        </div>
        {scope === 'circle' && circles.data && circles.data.length > 1 && (
          <div className={chipGroupClass} role="radiogroup" aria-label="Circle" style={{ marginTop: 12 }}>
            {circles.data.map((ci) => (
              <Chip key={ci.id} role="radio" selected={cid === ci.id} onClick={() => setCircleId(ci.id)}>
                <span aria-hidden>{ci.emoji}</span> {ci.name}
              </Chip>
            ))}
          </div>
        )}

        <div id="impact-panel" className={s.panel}>
          {impact.isPending ? (
            <div className="stack" aria-hidden>
              <Skeleton h={140} />
              <Skeleton h={260} />
            </div>
          ) : impact.isError ? (
            <Panel>
              <ErrorState error={impact.error} onRetry={() => impact.refetch()} />
            </Panel>
          ) : (
            d && (
              <>
                <Panel className={s.hero}>
                  <Bubble size={84} color="#2E9A3B" seed={scope}>
                    <Leaf size={40} weight="fill" aria-hidden />
                  </Bubble>
                  <div className={s.heroText}>
                    <span className={s.heroLabel}>{who} kept this much CO2 out of the air</span>
                    <span className={`${c.big} num co2`}>{grams(d.co2_saved_g)}</span>
                    {d.includes_demo_history && <span className={c.pill}>Includes demo history</span>}
                  </div>
                </Panel>
                <div className={s.stats}>
                  <Panel pad="tight" className={s.stat}>
                    <PersonSimpleWalk size={24} color="var(--sky-700)" aria-hidden />
                    <span className={`${s.statNum} num`}>{d.deliveries}</span>
                    <span className={s.statLabel}>walked deliveries</span>
                  </Panel>
                  <Panel pad="tight" className={s.stat}>
                    <Car size={24} color="var(--sky-700)" aria-hidden />
                    <span className={`${s.statNum} num`}>{d.car_miles_avoided.toLocaleString('en-US', { maximumFractionDigits: 1 })}</span>
                    <span className={s.statLabel}>car miles avoided</span>
                  </Panel>
                </div>
                <Panel>
                  <h2 className={s.h2}>CO2 saved each week</h2>
                  <p className="muted" style={{ fontSize: 'var(--fs-sm)', marginBottom: 8 }}>
                    Last {d.by_week.length} weeks. Bigger, higher bubbles mean more grams saved.
                  </p>
                  <WeeklyBubbles weeks={d.by_week} />
                </Panel>
                <Panel pad="tight" className={s.method}>
                  <h2 className={s.h2}>How we count it</h2>
                  <p>{d.method}</p>
                  {d.includes_demo_history && <p className="muted">Past weeks include seeded demo history so the chart has something to show at the hackathon.</p>}
                </Panel>
              </>
            )
          )}
        </div>
      </Page>
    </>
  )
}
