import type { SupabaseClient } from '@supabase/supabase-js'
import { api } from '../api/endpoints'
import type { PublicConfig } from '../api/types'
import { shrinkToJpeg } from './image'

// supabase-js is used for exactly one job: uploading the runner's receipt
// photo with a one-time signed token from our API. No tables, auth or realtime.
let client: SupabaseClient | null = null
let clientFor = ''

async function getClient(cfg: NonNullable<PublicConfig['supabase']>) {
  if (client && clientFor === cfg.url) return client
  const { createClient } = await import('@supabase/supabase-js')
  client = createClient(cfg.url, cfg.publishable_key, {
    auth: { persistSession: false, autoRefreshToken: false, detectSessionInUrl: false },
  })
  clientFor = cfg.url
  return client
}

export async function uploadReceipt(runId: string, file: File, cfg: NonNullable<PublicConfig['supabase']>) {
  const [blob, ticket, sb] = await Promise.all([shrinkToJpeg(file), api.receiptUpload(runId), getClient(cfg)])
  const { error } = await sb.storage.from(ticket.bucket).uploadToSignedUrl(ticket.path, ticket.token, blob, {
    contentType: 'image/jpeg',
  })
  if (error) throw new Error('The photo didn’t upload. Try again, or mark it paid without a photo.')
  return ticket.path
}
