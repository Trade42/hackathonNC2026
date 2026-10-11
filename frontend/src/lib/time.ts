import { useEffect, useState } from 'react'

// A clock that re-renders every `ms`. Pauses while the tab is hidden.
export function useNow(ms = 1000) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    let id: number | undefined
    const start = () => {
      window.clearInterval(id)
      setNow(Date.now())
      id = window.setInterval(() => setNow(Date.now()), ms)
    }
    const onVis = () => (document.hidden ? window.clearInterval(id) : start())
    start()
    document.addEventListener('visibilitychange', onVis)
    return () => {
      window.clearInterval(id)
      document.removeEventListener('visibilitychange', onVis)
    }
  }, [ms])
  return now
}
