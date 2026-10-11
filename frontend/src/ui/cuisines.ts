import { Basket, BowlSteam, Coffee, Cookie, ForkKnife, Hamburger, Pepper, type Icon } from '@phosphor-icons/react'
import type { AeroTone } from './AeroIcon'
import type { Family } from './CuisineArt'

// Cuisine shortcuts for Home and Search. Only families some restaurant has are shown.
export const CUISINES: { family: Family; label: string; icon: Icon; tone: AeroTone }[] = [
  { family: 'taco', label: 'Mexican', icon: Pepper, tone: 'sky' },
  { family: 'burger', label: 'Burgers', icon: Hamburger, tone: 'grass' },
  { family: 'bowl', label: 'Noodles', icon: BowlSteam, tone: 'aqua' },
  { family: 'pita', label: 'Mediterranean', icon: ForkKnife, tone: 'grass' },
  { family: 'cookie', label: 'Dessert', icon: Cookie, tone: 'sky' },
  { family: 'bagel', label: 'Breakfast', icon: Coffee, tone: 'aqua' },
  { family: 'basket', label: 'Grocery', icon: Basket, tone: 'grass' },
]
