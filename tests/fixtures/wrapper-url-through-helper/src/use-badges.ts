import { signedFetch } from './net/signed'
import { twoReturnFetch } from './net/two-way'
import { plainFetch, passFetch, regionFetch, badgeById } from './net/plain'

export function readBadge(): Promise<unknown> {
  return signedFetch('/badge')
}

export function dropBadge(): Promise<unknown> {
  return signedFetch('/badge', { method: 'DELETE' })
}

export function readTicketBadge(): Promise<unknown> {
  return twoReturnFetch('/badge')
}

export function readPlainBadge(): Promise<unknown> {
  return plainFetch('/badge')
}

export function readPassedBadge(): Promise<unknown> {
  return passFetch('/badge')
}

export function readRegionBadge(): Promise<unknown> {
  return regionFetch('eu')
}

export function readBadgeById(): Promise<unknown> {
  return badgeById('b1')
}
