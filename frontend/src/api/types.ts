// Contract types, generated from contract/openapi.yaml (npm run sync:contract).
// These aliases keep the app's names stable; never hand-edit the shapes.
import type { ProfileExtra, RunPlan } from './pending'
import type { components } from './schema'

type S = components['schemas']

export type LatLng = S['LatLng']
export type Place = S['Place']
export type PublicUser = S['PublicUser']
export type Weekday = S['Weekday']
export type UsualTime = S['UsualTime']
export type WalkingPace = S['WalkingPace']
export type Profile = S['Profile'] & ProfileExtra
export type User = Omit<S['User'], 'profile'> & { profile: Profile }
export type Area = S['Area']
export type Restaurant = S['Restaurant']
export type MenuChoice = S['MenuChoice']
export type MenuOption = S['MenuOption']
export type MenuItem = S['MenuItem']
export type Menu = S['Menu']
export type RunStatus = S['RunStatus']
export type RunnerLocation = S['RunnerLocation']
export type Run = S['Run'] & RunPlan
export type OrderStatus = S['OrderStatus']
export type OrderLine = S['OrderLine']
export type Order = S['Order']
export type RunDetail = S['RunDetail'] & RunPlan
export type GeoLineString = S['GeoJSONLineString']
export type Track = S['Track']
export type Circle = S['Circle']
export type InviteStatus = S['InviteStatus']
export type Invite = S['Invite']
export type NotificationType = S['NotificationType']
export type Notification = S['Notification']
export type WalletTxnKind = S['WalletTxnKind']
export type WalletTxn = S['WalletTxn']
export type Wallet = S['Wallet']
export type ImpactScope = S['ImpactScope']
export type Impact = S['Impact']
export type PublicConfig = S['PublicConfig']
export type Persona = S['DemoPersona']
export type AuthStart = S['TelegramAuthChallenge']
export type AuthPoll = S['TelegramAuthStatus']
export type AuthSession = S['AuthSession']
export type MeStats = S['MeStats']
export type NewOrderLine = S['OrderLineInput']
export type FerryRequest = S['PickupRequest']
export type ReceiptUpload = S['ReceiptUpload']
export type ErrorCode = S['ErrorCode']

export type StreamEvent =
  | S['RunUpdatedEvent']
  | S['OrderUpdatedEvent']
  | S['RunnerLocationEvent']
  | S['NotificationCreatedEvent']
  | S['InviteUpdatedEvent']
  | S['WalletUpdatedEvent']
