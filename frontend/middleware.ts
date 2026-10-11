// Vercel Edge Middleware: gives the deep links the agent sends in Telegram
// (/r, /o, /i) their own link-preview text. The SPA itself is unchanged.
export const config = { matcher: ['/r/:path*', '/o/:path*', '/i/:path*'] }

const copy: Record<string, { title: string; description: string }> = {
  r: { title: 'Join this ferry on Ferry', description: 'A friend is heading out for food. Add your order before the window closes and they’ll walk it to you.' },
  o: { title: 'Your friend is bringing your food', description: 'Ferry shows where your friend is as they walk your food over. No car trip, so no car exhaust.' },
  i: { title: 'You’re invited to meet again', description: 'Your circle wants to get together. Tap to answer and Ferry starts the Telegram group.' },
}

const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')

export default async function middleware(request: Request) {
  const url = new URL(request.url)
  const kind = url.pathname.split('/')[1]
  const c = copy[kind]
  if (!c) return
  const res = await fetch(new URL('/index.html', url))
  if (!res.ok) return
  let html = await res.text()
  const set = (prop: string, value: string) => {
    const re = new RegExp(`(<meta property="${prop}" content=")[^"]*(")`)
    html = re.test(html) ? html.replace(re, `$1${esc(value)}$2`) : html.replace('</head>', `<meta property="${prop}" content="${esc(value)}" /></head>`)
  }
  set('og:title', c.title)
  set('og:description', c.description)
  set('og:url', url.href)
  set('og:image', new URL('/og.png', url).href)
  html = html.replace(/<title>[^<]*<\/title>/, `<title>${esc(c.title)}</title>`)
  return new Response(html, { headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'public, max-age=60' } })
}
