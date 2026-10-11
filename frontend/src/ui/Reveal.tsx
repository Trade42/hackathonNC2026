import { createElement, useEffect, useRef, useState, type CSSProperties, type HTMLAttributes, type ReactNode } from 'react'

// Fades and lifts its content in the first time it scrolls into view. CSS
// only (see base.css .reveal), so it costs nothing on the landing page.
export function Reveal({
  as = 'div',
  delay = 0,
  className,
  style,
  children,
  ...rest
}: { as?: 'div' | 'section' | 'li' | 'article' | 'header' | 'p'; delay?: number; children?: ReactNode } & HTMLAttributes<HTMLElement>) {
  const ref = useRef<HTMLElement>(null)
  const [shown, setShown] = useState(false)
  useEffect(() => {
    const el = ref.current
    if (!el) return
    if (typeof IntersectionObserver === 'undefined') {
      setShown(true)
      return
    }
    const io = new IntersectionObserver(
      ([e]) => {
        if (e.isIntersecting) {
          setShown(true)
          io.disconnect()
        }
      },
      { threshold: 0.12, rootMargin: '0px 0px -6% 0px' },
    )
    io.observe(el)
    return () => io.disconnect()
  }, [])
  return createElement(
    as,
    {
      ref,
      className: ['reveal', shown && 'is-in', className].filter(Boolean).join(' '),
      style: { '--reveal-delay': `${delay}s`, ...style } as CSSProperties,
      ...rest,
    },
    children,
  )
}
