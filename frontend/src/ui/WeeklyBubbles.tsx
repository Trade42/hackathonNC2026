import { motion } from 'motion/react'
import { useState } from 'react'
import type { Impact } from '../api/types'
import { grams, weekDate } from '../lib/format'
import { useReducedMotion } from '../lib/prefs'
import s from './WeeklyBubbles.module.css'

const W = 640
const H = 240
const PAD = { l: 44, r: 16, t: 24, b: 30 }
const MARK = '#3FA535' // leaf mark hue, validated 3:1+ against the panel

function niceMax(v: number) {
  if (v <= 0) return 1000
  const p = Math.pow(10, Math.floor(Math.log10(v)))
  const n = Math.ceil(v / p)
  return (n <= 2 ? 2 : n <= 5 ? 5 : 10) * p
}

const compact = (g: number) => (g >= 1000 ? `${(g / 1000).toLocaleString('en-US', { maximumFractionDigits: 1 })} kg` : `${g} g`)

// Weekly CO2 as rising bubbles: height and area both show grams saved.
export function WeeklyBubbles({ weeks }: { weeks: Impact['by_week'] }) {
  const reduced = useReducedMotion()
  const [hover, setHover] = useState<number | null>(null)
  const [table, setTable] = useState(false)
  const max = niceMax(Math.max(...weeks.map((w) => w.co2_saved_g), 0) * 1.25)
  const innerW = W - PAD.l - PAD.r
  const innerH = H - PAD.t - PAD.b
  const x = (i: number) => PAD.l + (innerW * (i + 0.5)) / weeks.length
  const y = (g: number) => PAD.t + innerH - (g / max) * innerH
  const rMax = Math.min(22, innerW / weeks.length / 2 - 4)
  const r = (g: number) => (g <= 0 ? 0 : Math.max(5, Math.sqrt(g / max) * rMax))
  const peak = weeks.reduce((best, w, i) => (w.co2_saved_g > weeks[best].co2_saved_g ? i : best), 0)
  const last = weeks.length - 1
  const ticks = [0, max / 2, max]

  return (
    <div className={s.wrap}>
      {!table ? (
        <svg className={s.svg} viewBox={`0 0 ${W} ${H}`} role="group" aria-label="CO2 saved each week, last 8 weeks">
          <g className={s.grid} aria-hidden>
            {ticks.map((t) => (
              <g key={t}>
                <line x1={PAD.l} x2={W - PAD.r} y1={y(t)} y2={y(t)} />
                <text className={s.tick} x={PAD.l - 8} y={y(t) + 4} textAnchor="end">
                  {compact(t)}
                </text>
              </g>
            ))}
            {weeks.map((w, i) =>
              i % 2 === last % 2 ? (
                <text key={w.week_start} className={s.tick} x={x(i)} y={H - 8} textAnchor="middle">
                  {weekDate(w.week_start)}
                </text>
              ) : null,
            )}
          </g>
          {weeks.map((w, i) => {
            const rr = r(w.co2_saved_g)
            const label = `Week of ${weekDate(w.week_start)}: ${grams(w.co2_saved_g)} of CO2 saved by ${w.deliveries} ${w.deliveries === 1 ? 'delivery' : 'deliveries'}`
            return (
              <g
                key={w.week_start}
                className={s.bubble}
                tabIndex={0}
                role="img"
                aria-label={label}
                onPointerEnter={() => setHover(i)}
                onPointerLeave={() => setHover(null)}
                onFocus={() => setHover(i)}
                onBlur={() => setHover(null)}
              >
                <circle className="hit" cx={x(i)} cy={y(w.co2_saved_g)} r={Math.max(rr + 6, 18)} fill="transparent" />
                {rr > 0 && (
                  <motion.g
                    initial={reduced ? false : { y: H - y(w.co2_saved_g), opacity: 0 }}
                    animate={{ y: 0, opacity: 1 }}
                    transition={{ type: 'spring', stiffness: 120, damping: 20, delay: reduced ? 0 : i * 0.04 }}
                  >
                    <circle className="mark" cx={x(i)} cy={y(w.co2_saved_g)} r={rr} fill={MARK} stroke="#fff" strokeWidth={2} />
                    <circle cx={x(i) - rr * 0.35} cy={y(w.co2_saved_g) - rr * 0.38} r={rr * 0.28} fill="#fff" opacity={0.75} />
                  </motion.g>
                )}
                {(i === last || (i === peak && peak !== last)) && rr > 0 && (
                  <text className={s.label} x={x(i)} y={y(w.co2_saved_g) - rr - 6} textAnchor="middle" aria-hidden>
                    {compact(w.co2_saved_g)}
                  </text>
                )}
              </g>
            )
          })}
        </svg>
      ) : (
        <table className={s.table}>
          <caption className="sr-only">CO2 saved each week</caption>
          <thead>
            <tr>
              <th scope="col">Week of</th>
              <th scope="col">Deliveries</th>
              <th scope="col">CO2 saved</th>
            </tr>
          </thead>
          <tbody>
            {weeks.map((w) => (
              <tr key={w.week_start}>
                <td>{weekDate(w.week_start)}</td>
                <td className="num">{w.deliveries}</td>
                <td className="num">{grams(w.co2_saved_g)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {hover !== null && !table && (
        <div className={s.tooltip} style={{ left: `${(x(hover) / W) * 100}%`, top: `${((y(weeks[hover].co2_saved_g) - r(weeks[hover].co2_saved_g)) / H) * 100}%` }} aria-hidden>
          <strong>{grams(weeks[hover].co2_saved_g)}</strong>
          Week of {weekDate(weeks[hover].week_start)}, {weeks[hover].deliveries} {weeks[hover].deliveries === 1 ? 'delivery' : 'deliveries'}
        </div>
      )}
      <button type="button" className={s.tableToggle} onClick={() => setTable(!table)} aria-pressed={table}>
        {table ? 'Show as bubbles' : 'Show as a table'}
      </button>
    </div>
  )
}
