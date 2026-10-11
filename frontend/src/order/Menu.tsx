import { Info, Plus } from '@phosphor-icons/react'
import { useState } from 'react'
import type { Menu as MenuT, MenuItem } from '../api/types'
import { money, tagLabel } from '../lib/format'
import { ItemSheet } from './ItemSheet'
import s from './Order.module.css'

const leafTags = new Set(['vegan', 'vegetarian'])

export function Menu({ menu, cartKey, disabled }: { menu: MenuT; cartKey: string; disabled?: boolean }) {
  const [item, setItem] = useState<MenuItem | null>(null)
  return (
    <div>
      {menu.prices_are_estimates && (
        <p className={s.note}>
          <Info size={18} aria-hidden />
          Prices are estimates. You pay what the receipt says, and any difference comes back to your wallet.
        </p>
      )}
      {menu.categories.map((cat) => (
        <section key={cat.id} className={s.cat} aria-labelledby={`cat-${cat.id}`}>
          <h2 id={`cat-${cat.id}`}>{cat.name}</h2>
          {cat.items.map((it) => (
            <button key={it.id} type="button" className={s.item} onClick={() => setItem(it)} disabled={disabled} aria-haspopup="dialog">
              <span className={s.itemMain}>
                <strong>{it.name}</strong>
                <span>{it.description}</span>
                {it.tags.length > 0 && (
                  <span className={s.tags}>
                    {it.tags.map((t) => (
                      <span key={t} className={`${s.tag} ${leafTags.has(t) ? s.tagLeaf : ''}`}>
                        {tagLabel(t)}
                      </span>
                    ))}
                  </span>
                )}
              </span>
              <span className={s.itemSide}>
                <span className={`${s.price} num`}>{money(it.price_cents)}</span>
                <span className={s.add} aria-hidden>
                  <Plus size={16} weight="bold" />
                </span>
              </span>
            </button>
          ))}
        </section>
      ))}
      <ItemSheet item={item} cartKey={cartKey} onClose={() => setItem(null)} />
    </div>
  )
}
