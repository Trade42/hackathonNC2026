import { Link } from 'react-router-dom'
import { Logo } from '../shell/Logo'
import { Empty } from '../ui/States'
import { Panel } from '../ui/Surface'
import { GelLink } from '../ui/GelButton'

export default function NotFound() {
  return (
    <main id="main" style={{ minHeight: '100dvh', display: 'grid', placeItems: 'center', padding: 16 }}>
      <Panel style={{ maxWidth: 440, width: '100%' }}>
        <div style={{ display: 'grid', justifyItems: 'center' }}>
          <Link to="/" aria-label="Ferry home">
            <Logo size={28} />
          </Link>
        </div>
        <Empty title="This page drifted off" action={<GelLink to="/home">Go home</GelLink>}>
          The link may be old, or the ferry already left.
        </Empty>
      </Panel>
    </main>
  )
}
