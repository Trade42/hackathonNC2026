const usd = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' })

export const money = (cents: number) => usd.format(cents / 100)
export const signedMoney = (cents: number) => (cents > 0 ? '+' : cents < 0 ? '-' : '') + usd.format(Math.abs(cents) / 100)

export function grams(g: number) {
  if (g >= 10_000) return `${(g / 1000).toFixed(g >= 100_000 ? 0 : 1)} kg`
  return `${Math.round(g).toLocaleString('en-US')} g`
}

export const miles = (m: number) => {
  const mi = m / 1609.34
  return mi < 0.1 ? `${Math.round(m * 3.281)} ft` : `${mi.toFixed(1)} mi`
}

export function countdown(ms: number) {
  if (ms <= 0) return '0:00'
  const s = Math.floor(ms / 1000)
  const m = Math.floor(s / 60)
  if (m >= 60) return `${Math.floor(m / 60)}h ${m % 60}m`
  return `${m}:${String(s % 60).padStart(2, '0')}`
}

export function ago(iso: string, now = Date.now()) {
  const s = Math.max(0, Math.round((now - Date.parse(iso)) / 1000))
  if (s < 60) return `${s} s ago`
  const m = Math.round(s / 60)
  if (m < 60) return `${m} min ago`
  const h = Math.round(m / 60)
  if (h < 24) return `${h} h ago`
  const d = Math.round(h / 24)
  return d === 1 ? 'yesterday' : `${d} days ago`
}

export const clock = (iso: string) => new Date(iso).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' })

export const dayTime = (iso: string) =>
  new Date(iso).toLocaleString('en-US', { weekday: 'short', hour: 'numeric', minute: '2-digit' })

export const shortDate = (iso: string) => new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })

export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`

// Contract data is lowercase and machine-shaped; these make it read well.
export const titleCase = (s: string) => s.replace(/(^|[\s-])(\p{L})/gu, (_, a: string, b: string) => a + b.toUpperCase())
export const cuisineLabel = (list: string[]) => list.map(titleCase).join(', ')
export const tagLabel = (t: string) => t.replace(/_/g, ' ')
// "11:00-04:00" reads as "11 AM to 4 AM"; anything else passes through with "to".
const time12 = (hm: string) => {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hm.trim())
  if (!m) return hm.trim()
  const h = Number(m[1]) % 24
  const suffix = h < 12 ? 'AM' : 'PM'
  const hh = h % 12 === 0 ? 12 : h % 12
  return m[2] === '00' ? `${hh} ${suffix}` : `${hh}:${m[2]} ${suffix}`
}
export const hoursLabel = (h: string) =>
  h
    .split(/\s*[–—-]\s*/)
    .map(time12)
    .join(' to ')

// ImpactWeek.week_start is Monday 00:00 UTC; show that calendar date, not the local one.
export const weekDate = (iso: string) =>
  new Date(iso.length <= 10 ? `${iso}T00:00:00Z` : iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' })
