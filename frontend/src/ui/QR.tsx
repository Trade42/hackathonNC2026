import { useEffect, useState } from 'react'

// QR codes are generated locally (no network) and lazy-load the encoder.
export function QR({ value, size = 168, label }: { value: string; size?: number; label: string }) {
  const [svg, setSvg] = useState<string>('')
  useEffect(() => {
    let alive = true
    import('qrcode').then((q) =>
      q.toString(value, { type: 'svg', margin: 1, errorCorrectionLevel: 'M', color: { dark: '#0F2E44', light: '#FFFFFF' } }).then((out) => {
        if (alive) setSvg(out)
      }),
    )
    return () => {
      alive = false
    }
  }, [value])
  return (
    <div
      role="img"
      aria-label={label}
      style={{ width: size, height: size, padding: 8, borderRadius: 'var(--r-inner)', background: '#fff', boxShadow: 'var(--shadow-panel)' }}
      dangerouslySetInnerHTML={svg ? { __html: svg } : undefined}
    />
  )
}
