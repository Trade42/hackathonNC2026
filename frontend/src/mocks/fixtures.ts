// Demo fixtures come straight from contract/fixtures/*.json (copied into
// src/contract by `npm run sync:contract`). Only mock-only extras live here.
import type { Area, Circle, Menu, Place, PublicUser, Restaurant, Run, User } from '../api/types'
import areasJson from '../contract/fixtures/areas.json'
import circlesJson from '../contract/fixtures/circles.json'
import menusJson from '../contract/fixtures/menus.json'
import placesJson from '../contract/fixtures/places.json'
import restaurantsJson from '../contract/fixtures/restaurants.json'
import runsJson from '../contract/fixtures/runs.json'
import usersJson from '../contract/fixtures/users.json'

const fullUsers = usersJson as unknown as User[]

const toPublic = (u: User): PublicUser => ({ id: u.id, display_name: u.display_name, avatar_seed: u.avatar_seed, bubble_color: u.bubble_color })

export const users: Record<string, PublicUser> = Object.fromEntries(fullUsers.map((u) => [u.id, toPublic(u)]))

// Fake E.164 numbers for the mock group-text compose link (fixtures mask theirs).
export const phones: Record<string, string> = {
  usr_judge: '+19195550101',
  usr_maya: '+19195550102',
  usr_dana: '+19195550103',
  usr_leo: '+19195550104',
}

export const places: Record<string, Place> = Object.fromEntries((placesJson as unknown as Place[]).map((p) => [p.id!, p]))

export const areas = areasJson as unknown as Area[]

export const restaurants = restaurantsJson as unknown as Restaurant[]

export const menus: Record<string, Menu> = Object.fromEntries((menusJson as unknown as Menu[]).map((m) => [m.restaurant_id, m]))

export const demoRunFixture = (runsJson as unknown as Run[])[0]

export const seedUsers = (): Record<string, User> => Object.fromEntries(structuredClone(fullUsers).map((u) => [u.id, u]))

export const seedCircles = (_now: number): Circle[] => structuredClone(circlesJson as unknown as Circle[])

// Walking route from Cosmic Cantina east on Franklin St, south on Raleigh St,
// then east on South Rd to Fetzer Gym. [lng, lat] like GeoJSON.
export const demoRoute: [number, number][] = [
  [-79.055094, 35.913211],
  [-79.0548, 35.9134],
  [-79.0533, 35.9136],
  [-79.0517, 35.9138],
  [-79.0505, 35.9139],
  [-79.0497, 35.9137],
  [-79.0494, 35.9124],
  [-79.0491, 35.9112],
  [-79.0489, 35.9101],
  [-79.048, 35.9097],
  [-79.0472, 35.9095],
  [-79.046905, 35.909152],
]

export const IMPACT_METHOD = 'EPA estimate: about 400 g CO2 per car mile avoided'
