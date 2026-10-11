import { AndroidLogo, AppleLogo, Basket, ChatCircleText, DownloadSimple, HandHeart, Leaf, PersonSimpleWalk, TelegramLogo, UsersThree } from '@phosphor-icons/react'
import { lazy, Suspense, useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { useConfig, usePublicImpact } from '../api/hooks'
import type { PublicConfig } from '../api/types'
import { grams, money, plural } from '../lib/format'
import { ferryFeeCents, platformShareCents, RUNNER_BASE_CENTS } from '../lib/pricing'
import { useReducedMotion } from '../lib/prefs'
import { detectDownload, platform } from '../platform'
import { Phone } from '../scene/Phone'
import { Scenery } from '../scene/Scenery'
import { SoapBubbles, type BubbleSpot } from '../scene/SoapBubbles'
import { Logo } from '../shell/Logo'
import { AeroIcon } from '../ui/AeroIcon'
import { AmbientBubbles } from '../ui/AmbientBubbles'
import { Avatar } from '../ui/Bubble'
import { GelAnchor, GelLink } from '../ui/GelButton'
import { QR } from '../ui/QR'
import { Reveal } from '../ui/Reveal'
import { SoundToggle } from '../ui/SoundToggle'
import s from './Landing.module.css'

// The APK ships with the website, so anyone can download it.
const APK = '/downloads/ferry.apk'

// The chat on the phone starts a moment after load, so it loads after first paint.
const ChatDemo = lazy(() => import('../scene/ChatDemo').then((m) => ({ default: m.ChatDemo })))

// A few small bubbles drifting in the margins of the lower sections.
const TG_BUBBLES: BubbleSpot[] = [
  { x: '94%', y: '18%', size: 'clamp(28px, 4vw, 64px)', depth: 0.6, delay: 0.2, drift: 12 },
  { x: '4%', y: '86%', size: 'clamp(22px, 3vw, 46px)', depth: 0.4, delay: 0.4, drift: 10 },
]
const GET_BUBBLES: BubbleSpot[] = [
  { x: '6%', y: '22%', size: 'clamp(30px, 4.5vw, 72px)', depth: 0.7, delay: 0.2, drift: 13 },
  { x: '93%', y: '70%', size: 'clamp(24px, 3.4vw, 52px)', depth: 0.5, delay: 0.5, drift: 11 },
]

// True once the element has scrolled into view (and stays true).
function useSeen<T extends Element>() {
  const ref = useRef<T>(null)
  const [seen, setSeen] = useState(false)
  useEffect(() => {
    const el = ref.current
    if (!el || typeof IntersectionObserver === 'undefined') return setSeen(true)
    const io = new IntersectionObserver(([e]) => {
      if (e.isIntersecting) {
        setSeen(true)
        io.disconnect()
      }
    }, { threshold: 0.3 })
    io.observe(el)
    return () => io.disconnect()
  }, [])
  return [ref, seen] as const
}

// Counts up to the live number once it's in view; jumps there under reduced motion.
function useCountUp(target: number | undefined, start: boolean, ms = 1400) {
  const reduced = useReducedMotion()
  const [value, setValue] = useState(0)
  useEffect(() => {
    if (target === undefined || !start) return
    if (reduced) return setValue(target)
    let raf = 0
    const t0 = performance.now()
    const step = (t: number) => {
      const p = Math.min(1, (t - t0) / ms)
      setValue(Math.round(target * (1 - Math.pow(1 - p, 3))))
      if (p < 1) raf = requestAnimationFrame(step)
    }
    raf = requestAnimationFrame(step)
    return () => cancelAnimationFrame(raf)
  }, [target, start, reduced, ms])
  return value
}

function Co2Orb() {
  const { data, isError } = usePublicImpact()
  const [ref, seen] = useSeen<HTMLDivElement>()
  const kg = data ? data.co2_saved_g / 1000 : undefined
  const shown = useCountUp(kg === undefined ? undefined : Math.round(kg * 10), seen)
  const thisWeek = data?.by_week.at(-1)?.deliveries ?? 0
  return (
    <div ref={ref} className={s.orb} aria-live="polite">
      {/* One bubble per walked delivery on campus this week. */}
      <AmbientBubbles count={thisWeek} />
      <span className={s.orbShine} aria-hidden />
      {data ? (
        <span className={s.orbText}>
          <span className={`${s.orbNumber} num`}>
            {(shown / 10).toLocaleString('en-US', { minimumFractionDigits: 1, maximumFractionDigits: 1 })}
            <span className={s.orbUnit}>kg</span>
          </span>
          <span className={s.orbLabel}>of CO2 kept out of the air on campus</span>
          <span className={s.orbSub}>
            by {plural(data.deliveries, 'walked delivery', 'walked deliveries')}
            {data.includes_demo_history ? ', including demo history' : ''}
          </span>
        </span>
      ) : isError ? (
        <span className={s.orbLabel}>The campus counter is offline right now.</span>
      ) : (
        <>
          <span className={s.orbSkeleton} aria-hidden />
          <span className="sr-only">Loading the campus CO2 counter</span>
        </>
      )}
    </div>
  )
}

const steps = [
  {
    icon: ChatCircleText,
    tone: 'sky' as const,
    title: 'A student opens a trip',
    body: 'Someone already heading to a restaurant opens a short pickup window. You can join without knowing them.',
    quote: { who: 'Maya', text: 'heading to Cosmic Cantina at 6, anyone want anything?' },
  },
  {
    icon: Basket,
    tone: 'aqua' as const,
    title: 'You order through Ferry',
    body: 'Pick your food and drop-off. See the fee and tip before ordering; Ferry holds the total until handoff.',
    quote: { who: 'Ferry', text: 'Ferry to Cosmic Cantina is open until 6:10.', agent: true },
  },
  {
    icon: PersonSimpleWalk,
    tone: 'grass' as const,
    title: 'They walk it over and earn',
    body: 'Follow the walk, meet nearby, and confirm handoff. The captain gets base pay plus your entire tip.',
    quote: { who: 'Ferry', text: 'Maya has your food and is 4 min away (0.3\u00a0mi).', agent: true },
  },
]

// The route draws as the three cards move through the viewport, in either direction.
function Route({ progress }: { progress: number }) {
  const d = 'M80 54 C 120 30, 140 54, 167 54 C 280 54, 360 104, 500 54 S 720 4, 833 54 C 870 66, 900 70, 930 60'
  return (
    <>
      <svg className={s.route} viewBox="0 0 1000 120" preserveAspectRatio="none" aria-hidden focusable="false">
        <path d={d} className={s.routeBed} pathLength={1} />
        <path d={d} className={s.routeGlow} pathLength={1} style={{ strokeDashoffset: 1 - progress }} />
      </svg>
      <span className={s.mobileRoute} aria-hidden>
        <span style={{ transform: `scaleY(${progress})` }} />
      </span>
    </>
  )
}

function HowFlow() {
  const reduced = useReducedMotion()
  const ref = useRef<HTMLDivElement>(null)
  const [progress, setProgress] = useState(0)
  const shownProgress = reduced ? 1 : progress

  useEffect(() => {
    if (reduced) return
    let frame = 0
    const update = () => {
      frame = 0
      const rect = ref.current?.getBoundingClientRect()
      if (!rect) return
      const start = window.innerHeight * 0.78
      const end = window.innerHeight * 0.3
      const distance = rect.height + start - end
      setProgress(Math.max(0, Math.min(1, (start - rect.top) / distance)))
    }
    const schedule = () => { if (!frame) frame = requestAnimationFrame(update) }
    update()
    window.addEventListener('scroll', schedule, { passive: true })
    window.addEventListener('resize', schedule)
    return () => {
      window.removeEventListener('scroll', schedule)
      window.removeEventListener('resize', schedule)
      cancelAnimationFrame(frame)
    }
  }, [reduced])

  return (
    <div ref={ref} className={s.stepsWrap}>
      <Route progress={shownProgress} />
      <ol className={s.steps}>
        {steps.map(({ icon, tone, title, body, quote }, i) => (
          <li key={title} className={s.step} data-active={shownProgress >= i / steps.length}>
            <AeroIcon icon={icon} tone={tone} size={76} className={s.stepIcon} />
            <h3><span className="sr-only">Step {i + 1}: </span>{title}</h3>
            <p>{body}</p>
            <p className={`${s.quote} ${quote.agent ? s.quoteAgent : ''}`}>
              <b>{quote.who}</b>{quote.text}
            </p>
          </li>
        ))}
      </ol>
    </div>
  )
}

function PaySplit() {
  const subtotal = 1500
  const fee = ferryFeeCents()
  const tip = 200
  const platform = platformShareCents(fee)
  return (
    <section className={s.pay} aria-labelledby="pay-h">
      <div className={s.section}>
        <Reveal className={s.sectionHead}>
          <h2 id="pay-h" className="display">A pickup that pays the person walking.</h2>
          <p>Every order has one flat $2.99 Ferry fee. The student ferry captain earns $2.00 per handoff, plus 100% of your optional tip.</p>
        </Reveal>
        <div className={s.payGrid}>
          <Reveal className={s.payExample}>
            <h3>A {money(subtotal)} food order</h3>
            <dl>
              <div><dt>Food</dt><dd className="num">{money(subtotal)}</dd></div>
              <div><dt>Ferry fee</dt><dd className="num">{money(fee)}</dd></div>
              <div><dt>Example tip</dt><dd className="num">{money(tip)}</dd></div>
            </dl>
            <p>Tax is added at checkout. Tips are optional, and the exact total appears before you place the order.</p>
          </Reveal>
          <Reveal delay={0.1} className={s.payExample}>
            <h3>Where that fee goes</h3>
            <div className={s.payTrack} aria-hidden><span style={{ width: `${(RUNNER_BASE_CENTS / fee) * 100}%` }} /></div>
            <dl>
              <div><dt>Captain base pay</dt><dd className="num">{money(RUNNER_BASE_CENTS)}</dd></div>
              <div><dt>Captain also gets the tip</dt><dd className="num">+{money(tip)}</dd></div>
              <div><dt>Ferry keeps before costs</dt><dd className="num">{money(platform)}</dd></div>
            </dl>
            <p>Ferry’s share goes toward processing and running the service. This demo uses sandbox money; no real payouts happen yet.</p>
          </Reveal>
        </div>
      </div>
    </section>
  )
}

function TelegramCard({ cfg }: { cfg: PublicConfig | undefined }) {
  const url = cfg?.agent.telegram_url ?? null
  const showQr = !platform.isPhone && !!url
  const members = [
    { display_name: 'Maya', bubble_color: '#E0683C', avatar_seed: 'maya' },
    { display_name: 'Dana', bubble_color: '#8A5CD0', avatar_seed: 'dana' },
    { display_name: 'Leo', bubble_color: '#2E9A3B', avatar_seed: 'leo' },
    { display_name: 'You', bubble_color: '#0C7A8A', avatar_seed: 'guest-1' },
  ]
  return (
    <div className={s.tgCard}>
      <div className={s.tgGroup}>
        <div className={s.tgGroupHead}>
          <AeroIcon icon={UsersThree} size={40} shape="orb" />
          <span>
            <strong>Hinton 4th floor</strong>
            <small>5 members</small>
          </span>
        </div>
        <ul className={s.members}>
          {members.map((m, i) => (
            <li key={m.display_name} style={{ '--i': i } as React.CSSProperties}>
              <Avatar user={m} size={30} label={false} />
              {m.display_name}
            </li>
          ))}
          <li className={s.ferryJoins} style={{ '--i': 4 } as React.CSSProperties}>
            <AeroIcon icon={TelegramLogo} size={30} shape="orb" />
            <span>
              Ferry <small>joined the group</small>
            </span>
          </li>
        </ul>
      </div>
      {showQr && (
        <div className={s.tgQr}>
          <span className={s.qrScan}>
            <QR value={url} label="QR code that opens Ferry in Telegram" />
          </span>
          <p>Scan with your phone’s camera to open Ferry in Telegram.</p>
        </div>
      )}
    </div>
  )
}

function Downloads({ cfg }: { cfg: PublicConfig | undefined }) {
  const detected = detectDownload()
  const android = cfg?.downloads.android ?? APK
  const cards = [
    {
      key: 'ios',
      icon: AppleLogo,
      tone: 'sky' as const,
      name: 'iPhone',
      note: 'Add Ferry to your Home Screen from Safari. It opens full screen, with your trips and orders one tap away.',
      action: (
        <GelLink to="/install/ios" icon={<AppleLogo size={20} weight="fill" aria-hidden />}>
          Set up on iPhone
        </GelLink>
      ),
    },
    {
      key: 'android',
      icon: AndroidLogo,
      tone: 'grass' as const,
      name: 'Android',
      note: 'Android 7 or later. Your browser asks you to allow the install.',
      action: (
        <GelAnchor href={android} rel="noopener" download={android.startsWith('/') ? 'Ferry.apk' : undefined} icon={<DownloadSimple size={20} weight="bold" aria-hidden />}>
          Download for Android
        </GelAnchor>
      ),
    },
  ]
  const ordered = detected === 'android' ? [cards[1], cards[0]] : cards
  return (
    <div className={s.downloads}>
      {ordered.map((c, i) => (
        <Reveal as="article" key={c.key} delay={i * 0.1} className={s.dl} data-detected={c.key === detected}>
          <AeroIcon icon={c.icon} tone={c.tone} size={72} float />
          <div className={s.dlText}>
            <h3>
              {c.name}
              {c.key === detected && <span className={s.yours}>Your device</span>}
            </h3>
            <p>{c.note}</p>
          </div>
          {c.action}
        </Reveal>
      ))}
    </div>
  )
}

export function Landing() {
  const { data: cfg } = useConfig()
  const { data: impact } = usePublicImpact()
  const tg = cfg?.agent.telegram_url ?? null
  const addToGroup = cfg?.agent.telegram_add_to_group_url ?? tg

  useEffect(() => {
    document.title = 'Ferry: food already on the way'
  }, [])

  // The bar turns to fuller glass once the page scrolls.
  const topRef = useRef<HTMLSpanElement>(null)
  const [scrolled, setScrolled] = useState(false)
  useEffect(() => {
    const el = topRef.current
    if (!el || typeof IntersectionObserver === 'undefined') return
    const io = new IntersectionObserver(([e]) => setScrolled(!e.isIntersecting))
    io.observe(el)
    return () => io.disconnect()
  }, [])

  return (
    <div className={s.page}>
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      {/* Above the hero (not inside it), so no section can paint over it. */}
      <span id="top" ref={topRef} className={s.topSentinel} aria-hidden />
      <nav className={s.nav} data-scrolled={scrolled} aria-label="Ferry">
        <a href="#top" className={s.brand} aria-label="Ferry">
          <Logo size={26} animate="once" />
        </a>
        <div className={s.navLinks}>
          <a href="#how">How it works</a>
          <a href="#telegram">Telegram</a>
          <a href="#impact">Impact</a>
        </div>
        <div className={s.navActions}>
          <SoundToggle className={s.iconBtn} />
          <GelAnchor href="#get" size="sm" className={s.navCta}>
            Get the app
          </GelAnchor>
        </div>
      </nav>
      <header className={s.hero}>
        <Scenery />
        <SoapBubbles />

        <div className={s.heroCopy} id="main">
          <h1 className={`display ${s.headline}`}>
            <span className={s.ink}>Someone’s already going.</span> <span className={`${s.ink} ${s.ink2}`}>Ferry it.</span>
          </h1>
          <p className={s.sub}>Join a student’s food run nearby, even if you haven’t met. Ferry handles the order, the handoff and the pay. The captain earns for the extra stop.</p>
          <div className={s.ctas}>
            <GelAnchor href="#get" size="lg" icon={<DownloadSimple size={22} weight="bold" aria-hidden />}>
              Get the app
            </GelAnchor>
            {/* Always rendered (no layout shift); points at the bot once config arrives. */}
            <GelAnchor href={tg ?? '#telegram'} size="lg" variant="white" rel="noopener" icon={<TelegramLogo size={22} weight="fill" color="#229ED9" aria-hidden />}>
              Message Ferry on Telegram
            </GelAnchor>
          </div>
        </div>

        <figure className={s.phoneWrap}>
          <Phone>
            <Suspense fallback={null}>
              <ChatDemo />
            </Suspense>
          </Phone>
          <figcaption className="sr-only">How a Ferry run plays out in a group chat</figcaption>
        </figure>
      </header>

      <section className={s.roles} aria-labelledby="roles-h">
        <div className={s.section}>
          <Reveal className={s.sectionHead}>
            <h2 id="roles-h" className="display">
              Who brings your food?
            </h2>
            <p>A student heading that way does. Ferry organizes the order, payment and handoff in the app; a group chat is optional.</p>
          </Reveal>
          <ol className={s.roleList}>
            <Reveal as="li" className={s.role}>
              <span className={s.roleArt}>
                <Avatar user={{ display_name: 'Maya', bubble_color: '#E0683C', avatar_seed: 'maya' }} size={64} label={false} />
                <AeroIcon icon={PersonSimpleWalk} tone="grass" size={34} shape="orb" className={s.roleBadge} />
              </span>
              <h3>A student walks it over</h3>
              <p>They were heading to the restaurant anyway. Join their trip and meet them near your dorm, class or desk.</p>
            </Reveal>
            <Reveal as="li" delay={0.1} className={s.role}>
              <span className={s.roleArt}>
                <AeroIcon icon={ChatCircleText} tone="sky" size={64} />
              </span>
              <h3>Ferry keeps it organized</h3>
              <p>In the app or your Telegram chat, Ferry collects orders, holds the money, shows where the captain is and gives you a handoff code.</p>
            </Reveal>
            <Reveal as="li" delay={0.2} className={s.role}>
              <span className={s.roleArt}>
                <AeroIcon icon={HandHeart} tone="aqua" size={64} />
              </span>
              <h3>You get your food</h3>
              <p>Meet the captain, show the code and take your food. The captain receives base pay and the full tip at handoff.</p>
            </Reveal>
          </ol>
        </div>
      </section>

      <section className={s.how} aria-labelledby="how">
        <div className={s.section}>
          <Reveal className={s.sectionHead}>
            <h2 id="how" className="display">
              Trips students already make become food runs.
            </h2>
            <p>Join a nearby trip, see the price before ordering and meet the captain for a quick handoff.</p>
          </Reveal>
          <HowFlow />
        </div>
      </section>

      <PaySplit />

      <section className={s.tg} id="telegram" aria-labelledby="telegram-h">
        <SoapBubbles spots={TG_BUBBLES} className={s.sectionBubbles} />
        <div className={`${s.section} ${s.tgGrid}`}>
          <Reveal className={s.tgCopy}>
            <h2 id="telegram-h" className="display">
              Bring Ferry into your group chat
            </h2>
            <p>Campus trips are open in the app. You can also add Ferry to a Telegram group to share a run with people you know, or message the bot yourself. The captain may be someone new to you.</p>
            <div className={s.ctas}>
              <GelAnchor href={addToGroup ?? '#telegram'} size="lg" rel="noopener" icon={<UsersThree size={22} weight="fill" aria-hidden />}>
                Add Ferry to your group chat
              </GelAnchor>
              <GelAnchor href={tg ?? '#telegram'} size="lg" variant="white" rel="noopener" icon={<TelegramLogo size={22} weight="fill" color="#229ED9" aria-hidden />}>
                Message Ferry on Telegram
              </GelAnchor>
            </div>
          </Reveal>
          <Reveal delay={0.1}>
            <TelegramCard cfg={cfg} />
          </Reveal>
        </div>
      </section>

      <section className={s.impact} id="impact" aria-labelledby="impact-h">
        <div className={`${s.section} ${s.impactGrid}`}>
          <Reveal className={s.orbWrap}>
            <Co2Orb />
          </Reveal>
          <Reveal delay={0.1} className={s.method}>
            <h2 id="impact-h" className="display">
              Every walked delivery is a car trip that didn’t happen.
            </h2>
            <div className={s.methodMath}>
              <AeroIcon icon={Leaf} tone="leaf" size={44} shape="orb" />
              <span className={s.methodEq}>
                <span>1 car mile avoided</span> <span aria-hidden>=</span>{' '}
                <strong className="num">about 400 g CO2</strong>
              </span>
            </div>
            <p>
              For every walked delivery we measure the driving distance from the restaurant to your drop-off and count the CO2 a car would have burned covering it. A Franklin St run to campus saves about {grams(480)}.
            </p>
            {impact?.method && <p className={s.fine}>{impact.method}.</p>}
            <p className={s.fine}>
              Source: U.S. EPA,{' '}
              <a href="https://www.epa.gov/greenvehicles/greenhouse-gas-emissions-typical-passenger-vehicle" rel="noopener">
                Greenhouse gas emissions from a typical passenger vehicle
              </a>
              .
            </p>
          </Reveal>
        </div>
      </section>

      <section className={s.get} id="get" aria-labelledby="get-h">
        <SoapBubbles spots={GET_BUBBLES} className={s.sectionBubbles} />
        <div className={s.section}>
          <Reveal className={s.sectionHead}>
            <h2 id="get-h" className="display">
              Get the app
            </h2>
            <p>Browse food by area, follow your walker on the map, and see the CO2 you saved.</p>
          </Reveal>
          <Downloads cfg={cfg} />
        </div>
      </section>

      <footer className={s.footer}>
        <svg className={`${s.wave} ${s.waveBack}`} viewBox="0 0 2880 80" preserveAspectRatio="none" aria-hidden focusable="false">
          <path d="M0 30 C 300 60 540 60 720 40 S 1140 0 1440 30 S 2040 60 2160 40 S 2580 0 2880 30 L2880 80 L0 80 Z" />
        </svg>
        <svg className={s.wave} viewBox="0 0 2880 80" preserveAspectRatio="none" aria-hidden focusable="false">
          <path d="M0 40 C 240 10 480 10 720 34 S 1200 70 1440 40 S 1920 10 2160 34 S 2640 70 2880 40 L2880 80 L0 80 Z" />
        </svg>
        <div className={s.footerInner}>
          <div className={s.footerRow}>
            <Logo size={24} light />
            <nav className={s.footerLinks} aria-label="Footer">
              <a href="#how">How it works</a>
              <a href="#telegram">Telegram</a>
              <a href="#get">Get the app</a>
              <Link to="/signin">Web demo</Link>
              <a href="https://github.com/Trade42/HackNC2026" rel="noopener">
                Source on GitHub
              </a>
            </nav>
          </div>
          <p>Built at HackNC 2026, UNC Chapel Hill. Payments use Capital One’s Nessie sandbox, so no real money moves.</p>
        </div>
      </footer>
    </div>
  )
}
