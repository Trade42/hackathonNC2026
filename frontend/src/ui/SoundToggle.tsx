import { SpeakerHigh, SpeakerSlash } from '@phosphor-icons/react'
import { setPrefs, usePrefs } from '../lib/prefs'
import { sound } from '../lib/sound'

export function SoundToggle({ className }: { className?: string }) {
  const { sound: on } = usePrefs()
  return (
    <button
      type="button"
      className={className}
      aria-pressed={on}
      aria-label={on ? 'Sound on. Turn off water-drop sounds' : 'Sound off. Turn on water-drop sounds'}
      onClick={() => {
        setPrefs({ sound: !on })
        if (!on) sound.preview()
      }}
    >
      {on ? <SpeakerHigh size={22} aria-hidden /> : <SpeakerSlash size={22} aria-hidden />}
    </button>
  )
}
