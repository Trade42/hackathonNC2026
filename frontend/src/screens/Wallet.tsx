import { ArrowCounterClockwise, Bank, CreditCard, HandCoins, Lock, PlusCircle, ShoppingBag } from '@phosphor-icons/react'
import { useMutation, useQueries, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, setWallet, useConfig, useWallet } from '../api/hooks'
import type { WalletTxn, WalletTxnKind } from '../api/types'
import { dayTime, money, signedMoney } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { GelButton } from '../ui/GelButton'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Wallet.module.css'

const kinds: Record<WalletTxnKind, { label: string; Icon: typeof Bank }> = {
  deposit: { label: 'Deposit', Icon: Bank },
  order_hold: { label: 'Hold', Icon: Lock },
  refund: { label: 'Refund', Icon: ArrowCounterClockwise },
  payout: { label: 'Payout', Icon: HandCoins },
  card_funding: { label: 'Card funding', Icon: CreditCard },
  card_purchase: { label: 'Card purchase', Icon: ShoppingBag },
}

type Group = { key: string; runId?: string; txns: WalletTxn[] }

function group(txns: WalletTxn[]): Group[] {
  const out: Group[] = []
  const byRun = new Map<string, Group>()
  for (const t of txns) {
    if (t.run_id) {
      let g = byRun.get(t.run_id)
      if (!g) {
        g = { key: t.run_id, runId: t.run_id, txns: [] }
        byRun.set(t.run_id, g)
        out.push(g)
      }
      g.txns.push(t)
    } else out.push({ key: t.id, txns: [t] })
  }
  return out
}

export default function WalletScreen() {
  const wallet = useWallet()
  const cfg = useConfig()
  const qc = useQueryClient()
  const topup = useMutation({ mutationFn: (n: number) => api.demoTopup(n), onSuccess: (w) => setWallet(qc, w) })
  const groups = wallet.data ? group(wallet.data.transactions) : []
  const runIds = groups.filter((g) => g.runId).map((g) => g.runId!)
  const runs = useQueries({ queries: runIds.map((id) => ({ queryKey: keys.run(id), queryFn: () => api.run(id), staleTime: 60e3 })) })
  const runName = (id: string) => runs[runIds.indexOf(id)]?.data?.restaurant.name

  useEffect(() => {
    document.title = 'Ferry Wallet'
  }, [])

  return (
    <>
      <TopBar title="Wallet" />
      <Page>
        {wallet.isPending ? (
          <div className="stack" aria-hidden>
            <Skeleton h={150} />
            <Skeleton h={80} />
            <Skeleton h={80} />
          </div>
        ) : wallet.isError ? (
          <Panel>
            <ErrorState error={wallet.error} onRetry={() => wallet.refetch()} />
          </Panel>
        ) : (
          <div className="stack">
            <Panel className={s.balance}>
              <span className={s.label}>Ferry Wallet balance</span>
              <span className={`${c.big} num`} aria-live="polite">
                {money(wallet.data.balance_cents)}
              </span>
              <span className={s.sub}>
                Sandbox money (Capital One Nessie). {wallet.data.account_label}.
              </span>
              {wallet.data.mode === 'nessie_mock' && <span className={c.pill}>Offline sandbox</span>}
              {cfg.data?.demo_mode && (
                <div className={s.topup}>
                  <GelButton variant="white" size="sm" busy={topup.isPending} icon={<PlusCircle size={18} aria-hidden />} onClick={() => topup.mutate(2000)}>
                    Add $20 demo money
                  </GelButton>
                  {topup.isError && (
                    <span className={c.error} role="alert">
                      {topup.error instanceof ApiError ? topup.error.message : 'Top-up failed.'}
                    </span>
                  )}
                </div>
              )}
            </Panel>

            <h2 className={s.h2}>Activity</h2>
            {groups.length === 0 ? (
              <Panel>
                <Empty icon={<Bank size={28} color="var(--sky-700)" aria-hidden />} title="No activity yet">
                  Holds, refunds and payouts from your ferries show up here.
                </Empty>
              </Panel>
            ) : (
              <ul className={s.groups}>
                {groups.map((g) => {
                  const net = g.txns.reduce((a, t) => a + t.amount_cents, 0)
                  return (
                    <Panel as="li" key={g.key} pad="tight" className={s.group}>
                      {g.runId && (
                        <div className={s.groupHead}>
                          <strong>{runName(g.runId) ? `Ferry to ${runName(g.runId)}` : 'Ferry run'}</strong>
                          <span className="num muted">Net {signedMoney(net)}</span>
                        </div>
                      )}
                      <ul className={s.txns}>
                        {g.txns.map((t) => {
                          const k = kinds[t.kind]
                          return (
                            <li key={t.id} className={s.txn}>
                              <span className={s.icon}>
                                <k.Icon size={18} aria-hidden />
                              </span>
                              <span className={s.txnMain}>
                                <strong>{k.label}</strong>
                                <span>{t.description}</span>
                                <time dateTime={t.at}>{dayTime(t.at)}</time>
                              </span>
                              <span className={`${s.amount} ${t.amount_cents > 0 ? s.plus : ''} num`}>{signedMoney(t.amount_cents)}</span>
                            </li>
                          )
                        })}
                      </ul>
                    </Panel>
                  )
                })}
              </ul>
            )}
          </div>
        )}
      </Page>
    </>
  )
}
