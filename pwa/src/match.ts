import type { DecidedBy, MatchDecision, MatchDecisionRequest, MatchSummary, MediatorKind } from './api-phase4'

// Match mediation (spec 4.8): what the inbox highlights and what the check screen sends.

/** Only open matches wait for someone; the server already decides who sees which queue. */
export const openMatches = (matches: MatchSummary[]): MatchSummary[] => matches.filter((m) => m.status === 'open')

export const queueKey = (kind: MediatorKind): string => `match.queue_${kind}`

/** The decision buttons stay off until the volunteer says the data was checked with the person (spec 4.8 item 2). */
export const canDecide = (checked: boolean, busy: boolean): boolean => checked && !busy

export function decisionRequest(decision: MatchDecision, decidedBy: DecidedBy, note: string): MatchDecisionRequest {
  const text = note.trim()
  return { decision, decided_by: decidedBy, ...(text ? { note: text } : {}) }
}

/** The link only opens when it is a WhatsApp one: the server builds it, but a link is never followed blindly. */
export const whatsappHref = (link: string | undefined): string | null => (link?.startsWith('https://wa.me/') ? link : null)
