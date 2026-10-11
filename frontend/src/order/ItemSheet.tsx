import { Minus, Plus } from '@phosphor-icons/react'
import { useEffect, useId, useState } from 'react'
import type { MenuItem } from '../api/types'
import { money } from '../lib/format'
import { cartStore } from '../state/cart'
import { GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { Sheet } from '../ui/Sheet'
import s from './Order.module.css'

export function Qty({ value, onChange, min = 1, max = 20, label }: { value: number; onChange: (v: number) => void; min?: number; max?: number; label: string }) {
  return (
    <span className={s.qty} role="group" aria-label={label}>
      <button type="button" onClick={() => onChange(value - 1)} disabled={value <= min} aria-label={value - 1 < 1 ? 'Remove' : 'One less'}>
        <Minus size={18} weight="bold" aria-hidden />
      </button>
      <output aria-live="polite" className="num">
        {value}
      </output>
      <button type="button" onClick={() => onChange(value + 1)} disabled={value >= max} aria-label="One more">
        <Plus size={18} weight="bold" aria-hidden />
      </button>
    </span>
  )
}

export function ItemSheet({ item, cartKey, onClose }: { item: MenuItem | null; cartKey: string; onClose: () => void }) {
  const [choices, setChoices] = useState<Record<string, string[]>>({})
  const [qty, setQty] = useState(1)
  const [note, setNote] = useState('')
  const [tried, setTried] = useState(false)
  const noteId = useId()

  // Fresh defaults each time an item opens: required single choices preselect the first.
  useEffect(() => {
    if (!item) return
    const init: Record<string, string[]> = {}
    for (const o of item.options) init[o.id] = o.min === 1 && o.max === 1 ? [o.choices[0].id] : []
    setChoices(init)
    setQty(1)
    setNote('')
    setTried(false)
  }, [item])

  if (!item) return <Sheet open={false} onClose={onClose} title="">{null}</Sheet>

  const chosen = item.options.flatMap((o) => choices[o.id] ?? [])
  const unit = item.price_cents + item.options.flatMap((o) => o.choices).filter((c) => chosen.includes(c.id)).reduce((a, c) => a + c.price_delta_cents, 0)
  const invalid = item.options.filter((o) => (choices[o.id]?.length ?? 0) < o.min)

  const add = () => {
    setTried(true)
    if (invalid.length) return
    cartStore.add(cartKey, {
      menu_item_id: item.id,
      name: item.name,
      qty,
      choice_ids: chosen,
      choice_names: item.options.flatMap((o) => o.choices).filter((c) => chosen.includes(c.id)).map((c) => c.name),
      unit_price_cents: unit,
      note: note.trim() || undefined,
    })
    onClose()
    // The item pops into the cart.
    requestAnimationFrame(() => pop(document.getElementById('cart-bar'), { label: `Added ${item.name}`, size: 44 }))
  }

  return (
    <Sheet
      open={!!item}
      onClose={onClose}
      title={item.name}
      subtitle={item.description}
      footer={
        <div className={s.sheetFoot}>
          <Qty value={qty} onChange={setQty} label="Quantity" />
          <GelButton onClick={add}>Add to order {money(unit * qty)}</GelButton>
        </div>
      }
    >
      {item.options.map((o) => {
        const single = o.max === 1
        const sel = choices[o.id] ?? []
        const err = tried && sel.length < o.min
        return (
          <fieldset key={o.id} className={s.option} aria-invalid={err || undefined}>
            <legend>
              {o.name}
              <small>{o.min > 0 ? (single ? 'Pick one' : `Pick at least ${o.min}`) : `Optional, up to ${o.max}`}</small>
            </legend>
            {o.choices.map((c) => {
              const checked = sel.includes(c.id)
              const full = !single && !checked && sel.length >= o.max
              return (
                <label key={c.id} className={s.choice}>
                  <input
                    type={single ? 'radio' : 'checkbox'}
                    name={o.id}
                    checked={checked}
                    disabled={full}
                    onChange={() =>
                      setChoices({
                        ...choices,
                        [o.id]: single ? [c.id] : checked ? sel.filter((x) => x !== c.id) : [...sel, c.id],
                      })
                    }
                  />
                  <span>{c.name}</span>
                  {c.price_delta_cents !== 0 && <span className="muted num">+{money(c.price_delta_cents)}</span>}
                </label>
              )
            })}
            {err && <p className="muted" style={{ color: 'var(--danger)', fontWeight: 600 }}>Choose an option for {o.name.toLowerCase()}.</p>}
          </fieldset>
        )
      })}
      <label htmlFor={noteId} style={{ fontWeight: 600, display: 'block', marginBottom: 6 }}>
        Note for your captain (optional)
      </label>
      <textarea id={noteId} className={s.textarea} value={note} maxLength={140} onChange={(e) => setNote(e.target.value)} placeholder="No onions, extra napkins" />
    </Sheet>
  )
}
