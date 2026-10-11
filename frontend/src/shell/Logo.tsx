import { RunnerMark } from '../ui/RunnerMark'

// The Ferry logo: the runner mark and the wordmark in our display serif.
export function Logo({
  size = 28,
  word = true,
  light,
  animate = false,
  className,
}: {
  size?: number
  word?: boolean
  light?: boolean
  animate?: 'loop' | 'once' | false
  className?: string
}) {
  return (
    <span className={className} style={{ display: 'inline-flex', alignItems: 'center', gap: size * 0.26 }}>
      <RunnerMark size={size * 1.12} animate={animate} />
      {word && (
        <span
          style={{
            fontFamily: 'var(--font-display)',
            fontSize: size * 1.04,
            lineHeight: 1,
            letterSpacing: '-0.01em',
            color: light ? '#fff' : 'var(--ink-display)',
          }}
        >
          Ferry
        </span>
      )}
    </span>
  )
}
