import { ArrowLeft, ArrowSquareUp, CheckCircle, House, Plus, TelegramLogo } from '@phosphor-icons/react'
import { useEffect } from 'react'
import { Link } from 'react-router-dom'
import { env } from '../config/env'
import { platform } from '../platform'
import { Scenery } from '../scene/Scenery'
import { Logo } from '../shell/Logo'
import { GelLink } from '../ui/GelButton'
import { QR } from '../ui/QR'
import { glassClass } from '../ui/Surface'
import s from './InstallIOS.module.css'

const steps = [
  {
    title: 'Open Ferry in Safari',
    body: <>Use Safari on your iPhone or iPad. If this page opened in another app, copy its address and open it in Safari.</>,
    icon: House,
  },
  {
    title: 'Open the Share menu',
    body: <>In Safari, tap <strong>Share</strong> (the square with an up arrow). If you see a Page Menu button instead, tap it, then tap Share.</>,
    icon: ArrowSquareUp,
  },
  {
    title: 'Choose Add to Home Screen',
    body: <>Scroll through the Share menu and tap <strong>Add to Home Screen</strong>. If it is missing, tap <strong>Edit Actions</strong> at the bottom and add it.</>,
    icon: Plus,
  },
  {
    title: 'Open as a web app',
    body: <>Turn on <strong>Open as Web App</strong>, then tap <strong>Add</strong>. Ferry will appear on your Home Screen with its own icon.</>,
    icon: CheckCircle,
  },
  {
    title: 'Open Ferry and sign in',
    body: <>Tap the Ferry icon, then sign in with Telegram. Check that the four-digit code matches and tap <strong>Confirm</strong> in the Ferry chat. You’ll stay signed in between app launches.</>,
    icon: TelegramLogo,
  },
]

export default function InstallIOS() {
  useEffect(() => {
    document.title = 'Install Ferry on iPhone'
  }, [])

  const installUrl = `${window.location.origin}/install/ios`
  const standalone = window.matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone === true

  return (
    <div className={s.page}>
      <Scenery variant="compact" />
      <header className={`${s.top} ${glassClass}`}>
        <Link to="/" aria-label="Ferry home"><Logo size={26} /></Link>
        <span>iPhone setup</span>
      </header>

      <main className={s.main} id="main">
        <div className={s.intro}>
          <p className={s.eyebrow}>Ferry for iPhone</p>
          <h1 className="display">Ferry, one tap away.</h1>
          <p>Add Ferry to your Home Screen in Safari. It opens full screen with the same trips, orders, tracking and wallet as the web app.</p>
          {standalone && <p className={s.installed}><CheckCircle size={20} weight="fill" aria-hidden /> You’re already using Ferry as a Home Screen app.</p>}
        </div>

        <div className={s.layout}>
          <section className={s.guide} aria-labelledby="install-steps">
            <h2 id="install-steps">Add it in five steps</h2>
            <ol className={s.steps}>
              {steps.map(({ title, body, icon: Icon }, index) => (
                <li key={title} className={s.step}>
                  <span className={s.number} aria-hidden>{index + 1}</span>
                  <div className={s.stepCopy}>
                    <div className={s.stepHead}><Icon size={22} weight="duotone" aria-hidden /><h3>{title}</h3></div>
                    <p>{body}</p>
                  </div>
                </li>
              ))}
            </ol>
            <p className={s.sessionNote}>Home Screen apps keep their own sign-in, separate from Safari. Sign in from the Ferry icon once, even if you already signed in on the website. Your session stays on this device until it expires or you sign out.</p>
          </section>

          <aside className={s.aside} aria-label="Ferry Home Screen preview">
            <div className={s.preview}>
              <div className={s.previewShine} aria-hidden />
              <div className={s.appIcon}>
                <img src="/apple-touch-icon.png" alt="" width="88" height="88" />
                <span>Ferry</span>
              </div>
              <p>Food already on the way.<br />Now right on your Home Screen.</p>
            </div>
            {!platform.isIOS && (
              <div className={s.qrBlock}>
                <QR value={installUrl} size={144} label="Scan to open the iPhone install instructions" />
                <div><strong>On your computer?</strong><span>Scan this with your iPhone camera, then follow the steps in Safari.</span></div>
              </div>
            )}
            <div className={s.after}>
              <h2>After you add it</h2>
              <p>Tap the Ferry icon for the full-screen app. Trip and order links from Telegram may open in Safari; they still lead to the right screen, but Safari can ask for its own sign-in.</p>
              {env.isMock && <p className={s.demoNote}>This preview uses demo accounts and sandbox money. No real payment is made.</p>}
              <GelLink to="/home" variant="white" size="sm">Open the web demo</GelLink>
            </div>
          </aside>
        </div>
        <Link to="/#get" className={s.back}><ArrowLeft size={18} aria-hidden /> Back to downloads</Link>
      </main>
    </div>
  )
}
