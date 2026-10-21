# Technical specification — Taba (Human Token for shelters and disaster rescue)

> English translation of `docs/pt_br/spec/spec.md` (spec version 2, 2026-10-21). In case of divergence, the Portuguese version prevails. Sources: the owner's decisions (items a–j, recorded in section 13), `docs/pt_br/base legal LGPD.md`, `infra/README.md`, a review by a disaster management specialist and the tests against the real API (these last three are internal documents, outside the repository), and the public documentation of Unico Human Token.
>
> Conventions of this document:
> - **Owner decision (a–j):** requirement. Cited as "[dec. a]" etc. Review findings accepted by the owner are cited as "[dec. h, A1]".
> - **PROPOSAL (to be confirmed):** my own solution for something the decisions do not cover. Every proposal appears again in section 12.
> - No marking: technical implementation choice, with the rationale in the decision log (section 13).
> - Technical reading of the LGPD, not a legal opinion.
> - The text is in Portuguese in the original; **every identifier is in English** (section 0).

## Contents

0. Naming conventions and implementation language
1. Vision, goals and out of scope
2. Actors, roles and permission matrix
3. Features and flows
4. Business rules
5. Integration with Unico
6. Architecture and deployment
7. Data model (Firestore)
8. REST API
9. Offline synchronization and conflicts
10. Security and privacy
11. Interface languages (i18n)
12. Open questions and proposals to be confirmed
13. Decision log
14. Repository file structure
15. Acceptance criteria

---

## 0. Naming conventions and implementation language [dec. j]

- **Everything in the implementation is in English:** code, identifiers, API routes, payloads, Firestore collections and fields, state values, roles, environment variables, crates, modules, directories, log messages, **commit messages and pull request descriptions**.
- The user interface is translated (section 11). The project documentation remains in Portuguese.
- One name per concept. Reference table:

| Concept (pt) | Identifier (en) |
|---|---|
| Taba (project, binary, service) | `taba` |
| evento (event) | `event` / collection `events` |
| abrigo (shelter) | `shelter` / `shelters` |
| voluntário (volunteer, profile) | `volunteer` / `volunteers` |
| vínculo de membro (membership link) | `membership` / `memberships` |
| role in the shelter: admin, voluntário (volunteer) | `admin`, `volunteer` |
| global roles: validador (validator), Defesa Civil (Civil Defense) | `validator`, `civil_defense` |
| convite (invite) | `invite` / `invites` |
| aval (endorsement: in person, remote) | `endorsement` (`in_person`, `remote`) |
| código de aval presencial (in-person endorsement code) | `endorsement_codes` |
| sessão (session) | `session` / `sessions` |
| captura (capture, Unico journey) | `capture` / `captures` |
| pessoa atendida (person served) | `person` / `people` |
| level: verificado, assistido, sem biometria (verified, assisted, no biometrics) | `verified`, `assisted`, `no_biometrics` |
| passagem por abrigo (stay at a shelter) | `stay` / `stays` |
| responsável por criança (child's guardian) | `guardian` / `guardian_ids` |
| pulseira (wristband, improvised number) | `wristband` |
| cartão/pulseira com QR do núcleo familiar (household card/wristband with QR) | `card` / `cards` |
| doação (donation, delivery) | `delivery` / `deliveries` |
| item de doação (donation item) | `item` |
| resgate (rescue) | `rescue` / `rescues` |
| busca de desaparecido (missing person search) | `search` / `searches` |
| match | `match` / `matches` |
| fusão de abrigos (shelter merge) | `merge` / `merges` |
| alerta (alert) | `alert` / `alerts` |
| auditoria (audit) | `audit_log` |
| miniatura (thumbnail) | `thumbnail` |
| "Estou Seguro" ("I'm Safe") | `safe_check` |
| não confirmado / confirmado (unconfirmed / confirmed) | `unconfirmed` / `confirmed` |
| aberto / encerrado / fundido (open / closed / merged; shelter) | `open` / `closed` / `merged` |
| ativo / encerrado / fundido / descartado (active / closed / merged / purged; event) | `active` / `closed` / `merged` / `purged` |
| descarte (purge) | `purge` |
| sinalizado (flagged) | `flagged` |
| em revisão (in review) | `in_review` |
| painel (dashboard) | `dashboard` |
| idempotência (idempotency) | `idempotency` |

---

## 1. Vision, goals and out of scope

### 1.1 Vision
Open source system for shelters and rescue teams in disasters (reference: the Rio Grande do Sul (RS) floods, 2024). It recognizes people by their face, without documents, using Unico's Human Token: the same person always receives the same token. The token does not say who the person is, only that they are the same as before. The system **does not validate identity** and must not be presented as doing so.

### 1.2 v1 goals
1. A single registry of people served across shelters, with a history that follows the person.
2. Donation delivery by household card, with flagging (no blocking) of repeated pickups [dec. h, A1].
3. Registration of rescued people at the landing or triage point, linked to the shelter later [dec. h, A11].
4. Volunteer accreditation with liveness.
5. Search for missing people by photo, mediated, never returning the location to the person who searched.
6. "Estou Seguro" ("I'm Safe"): self-registration from the phone; whoever needs rescue is told to call 193/199 [dec. h, C3].
7. Revalidation when a child leaves and on high-value or randomly sampled deliveries [dec. h, A1].
8. Work without internet at the shelter, except what depends on Human Token with liveness.
9. Hosting cost within the GCP free tier, except the external IPv4 and the egress traffic of a real event [dec. g, i].

### 1.3 Out of scope for v1
- Native app [dec. a]; integration with S2iD and Cemaden [dec. c].
- Spanish in the rest of the interface (only consent and "Estou Seguro" have Spanish) [dec. h, M3].
- "I need rescue" alert stored in the system [dec. h, C3]. Comes back when an operations center commits to monitoring the queue.
- Rescue registration inside the boat [dec. h, A11].
- Health records, emergency benefit screening, damage registry, volunteer hours and certificate, selfie at the start of each shift (ideas 4–6 of the history).
- Push notification. **PROPOSAL (to be confirmed):** notices appear on the "Avisos" (Notices) screen when the app is opened (Q19).
- Missing person search done by the family itself, without an operator. **PROPOSAL (to be confirmed)** (Q8).
- Web backoffice: global roles are granted via command line on the VM (2.1).

---

## 2. Actors, roles and permission matrix

### 2.1 Actors

| Actor | How it exists in the system | Authentication |
|---|---|---|
| **Person served** | Document in `people`. Does not use the app (except for "Estou Seguro"). | None; recognized by face or by card. |
| **Family** | Contact data inside a `search`. Does not use the app. | None. |
| **Volunteer** | Document in `volunteers` + `memberships` with `role = volunteer`. Anyone who has done Human Token with liveness has a profile [dec. b]. Includes the landing point operator [dec. h, A11]. | Session issued after a journey with liveness. |
| **Shelter admin** | `memberships` with `role = admin`. The shelter creator is the initial admin [dec. a]. | Same as volunteer. |
| **Validator** | `volunteers.global_roles` contains `validator` (Red Cross, city governments, Civil Defense (Defesa Civil)). Granted once, not per event [dec. d]. | Same as volunteer. |
| **Civil Defense** | `volunteers.global_roles` contains `civil_defense`. | Same as volunteer. |
| **Instance operator** | Whoever administers the VM. Not a role in the app. | SSH on the VM. |

**PROPOSAL (to be confirmed) — global roles (Q23):** `civil_defense` includes all the powers of `validator` plus: ban from the network, remove an admin, view the dashboard, mediate matches from unconfirmed shelters and of people outside shelters. `validator` confirms and revokes shelters and views the dashboard (read only). Both are granted and withdrawn by the instance operator with `taba role grant|revoke <volunteer_id> <role>`, which writes an audit record.

### 2.2 Shelter trust levels [dec. d]

| `confirmation` / `confirmation_type` | How it is reached | `can_endorse` |
|---|---|---|
| `unconfirmed` | Creation; revocation | `false` |
| `confirmed` / `validator` | Validator confirms | `true` |
| `confirmed` / `in_person` | Admin of a shelter that can endorse, phones ≤ 100 m apart | `true` |
| `confirmed` / `remote` | Two admins of two shelters that can endorse, same event | `false`, until it receives an in-person or validator endorsement |

### 2.3 Permission matrix

Legend: V = shelter volunteer; A = shelter admin; Val = validator; DC = Civil Defense; NC = `unconfirmed` shelter; C = `confirmed` shelter. "Own" = of the shelter in which the actor is an active member.

| Operation | V (NC) | V (C) | A (NC) | A (C) | Val | DC |
|---|---|---|---|---|---|---|
| Create shelter (any profile with liveness) | yes | yes | yes | yes | yes | yes |
| Check-in, card issuance, delivery, exit at own shelter | yes | yes | yes | yes | — | — |
| View own registrations | yes | yes | yes | yes | — | — |
| View other shelters' history of a person present at own shelter | no | yes | no | yes | — | — |
| Register rescue (landing point) | yes | yes | yes | yes | — | yes |
| Register search by photo on the network | no | yes | no | yes | — | yes |
| Mediate match (person at own shelter) | no | no | no | yes | — | yes (DC queue) |
| View thumbnail of unidentified person at own shelter | yes | yes | yes | yes | — | only when mediating |
| Review flagged delivery | no | no | yes | yes | — | — |
| Configure items (period, high value) | no | no | yes | yes | — | — |
| Invite volunteer (QR or link) | no | no | yes | yes | — | — |
| Invite admin (in-person QR only) | no | no | yes | yes | — | — |
| Promote volunteer to admin | no | no | yes | yes | — | — |
| Remove volunteer from shelter | no | no | yes | yes | — | yes |
| Remove admin | no | no | no | no | — | yes |
| Ban from the network | no | no | no | no | — | yes |
| Request merge with another shelter (offline conflict only) | no | no | yes | yes | — | — |
| Accept merge into own shelter | no | no | no | yes | — | — |
| Export own shelter's data | no | no | no | yes | — | — |
| In-person / remote endorsement | no | no | no | yes, if `can_endorse` | — | — |
| Confirm or revoke shelter | no | no | no | no | yes | yes |
| Network dashboard | no | no | no | no | read | yes |
| Rename event | no | no | yes | yes | — | yes |
| Close own shelter | no | no | yes | yes | — | yes |

Membership rules [dec. b]: an admin does not remove an admin; the shelter always has at least one active admin; the last admin only leaves after promoting another. An admin may leave if they are not the last one.

---

## 3. Features and flows

"Capture with liveness" = the app requests a capture from the server, opens Unico's journey link (flow `humantoken`), the person takes the selfie, the app returns and the server obtains the token (5.2). Requires internet [dec. i, Q4].

"Photo from the app camera" = `getUserMedia` + frame captured in a `canvas`, with no gallery access (history, "Use without liveness", item 4). Produces level `assisted`.

### 3.0 Device prerequisites
1. First access: the app generates `device_id` (UUID v4) and stores it in IndexedDB.
2. On iPhone, the first screen asks to install to the home screen before any registration [dec. a]. The app detects `navigator.standalone !== true` on iOS and blocks shelter creation and offline registrations until installation.
3. On install or sign-in, the app calls `navigator.storage.persist()` [dec. a]. If the answer is `false`, it shows a permanent warning "os dados deste celular podem ser apagados pelo navegador; sincronize com frequência" ("this phone's data may be deleted by the browser; sync often").

### 3.1 Volunteer accreditation and login (feature 4)
1. "Entrar" (Sign in) screen. The app calls `POST /v1/captures {purpose: "login"}` and receives `{capture_id, link}`.
2. The app stores `capture_id` in IndexedDB and navigates to `link`.
3. The journey redirects to `https://<domain>/return?capture=<id>`. The app calls `POST /v1/sessions {capture_id}`.
4. Profile exists: receives the session. Does not exist: `422 profile_required`; the app shows the form (name, optional phone, "mostrar meu telefone a quem tentar criar abrigo perto do meu" ("show my phone to whoever tries to create a shelter near mine") [dec. a], language) and resends `POST /v1/sessions {capture_id, profile}`.
5. Banned profile: `403 banned`.
6. Offline: "Entrar" is disabled with the message "precisa de internet" ("needs internet").

### 3.2 Shelter creation [dec. a, b, c]
1. Precondition: valid session (any volunteer).
2. The app reads `navigator.geolocation.getCurrentPosition({enableHighAccuracy: true, timeout: 30000, maximumAge: 0})` and stores `lat`, `lon`, `accuracy`.
3. `accuracy > 100 m`: the app asks the user to go to an open area and try again.
4. Online: the app calls Nominatim (reverse) directly from the phone and fills in the address. Offline: it keeps the coordinates with `address.pending = true` and looks it up later (6.1.6).
5. The operator types the name or nickname. Photos of the site (without people) are optional; **PROPOSAL (to be confirmed):** up to 2, reduced to ≤ 200 KB (Q29).
6. Online: `GET /v1/events/nearby?lat&lon`. If there are active events in the region or state and none within ≤ R km, the app asks "Este abrigo recebe pessoas de algum destes eventos?" ("Does this shelter receive people from any of these events?") [dec. c]. **PROPOSAL (to be confirmed):** "region or state" = events with some shelter in the same state (UF) or within ≤ 300 km (Q31).
7. The app generates `shelter_id` (UUID v4) and sends `POST /v1/shelters` (directly or through the queue).
8. Server, under the global creation lock (6.2.4):
   1. Validates `accuracy ≤ 100`.
   2. Looks for `open` shelters within ≤ `SHELTER_RADIUS_M`. If it finds one and the request does not carry `other_site_reason`: `409 shelter_nearby` with `{shelter_id, name, distance_m, contact: {first_name, phone?}}`. `phone` only if the creator checked `show_phone`.
   3. With `other_site_reason`: creates with `flagged = true` and a `shelter_flagged` alert for Civil Defense.
   4. Assigns the event (4.3).
   5. Creates the shelter as `unconfirmed`, the creator's `membership` as `admin`, and writes the audit record.
9. On the `409` response, the app shows "Já existe o abrigo X; procure <primeiro nome>" ("Shelter X already exists; look for <first name>") and two ways out: "É outro local? Explique." ("Is it a different site? Explain.") (resends with `other_site_reason`) and, if the shelter was created offline and already has registrations, "Fundir meus cadastros no abrigo X" ("Merge my registrations into shelter X") (3.2.1).

#### 3.2.1 Offline conflict and merge [dec. a, i]
1. Two shelters created with no signal < 100 m apart: the second one to sync receives the `409`.
2. That shelter's queue items become `held` (9.3).
3. The admin chooses "Explique" (Explain) (it becomes a `flagged` shelter, the items proceed) or "Fundir" (Merge).
4. "Fundir" creates the source shelter on the server as `unconfirmed` (to receive the held items) and a request in `merges`. An admin of the destination shelter accepts or rejects [dec. i]. On acceptance, the server moves people, cards, deliveries and rescues to the destination (the token resolves duplicates, 9.4), turns the source's members into `volunteer` of the destination and marks the source `status = merged`. Before accepting, the destination sees nothing of the source, and the source never sees anything of the destination other than the name and the creator's first name.

### 3.3 Volunteer invitation [dec. b]
1. Admin taps "Convidar voluntário" (Invite volunteer), chooses the level (`volunteer` by default, or `admin`) and the form:
   - **In-person QR** (`kind = qr`): single use, expires in 30 min, level `admin` or `volunteer`.
   - **Remote link** (`kind = link`): single use, expires in 24 h, **`volunteer` only**.
2. `POST /v1/shelters/{id}/invites` → `{code, url, expires_at}`. QR and link carry only `https://<domain>/invite/<code>`. Requires internet.
3. The invitee opens the URL; the app shows `GET /v1/invites/{code}` (shelter name, level, whether it is confirmed).
4. The invitee always does a capture with liveness (`purpose: "invite"`). Whoever already has a profile only does this capture, which is the revalidation [dec. b]; whoever does not fills in the profile.
5. `POST /v1/invites/{code}/accept {capture_id, profile?}` → creates the `membership` and returns the session.
6. The admin sees active invites and cancels them (`DELETE /v1/invites/{code}`). Whoever invited sees who joined via the link and can remove them.
7. Rejections: invite expired, used or cancelled (`410 invite_invalid`); banned profile (`403 banned`); already an active member (`409 already_member`); previously removed from that shelter (`403 removed_from_shelter`; **PROPOSAL (to be confirmed)**: only returns via in-person QR, Q37).

### 3.4 Shelter check-in (feature 1)
Check-in is the moment the person is checked [dec. h, A1].

**Online, conscious person:**
1. Volunteer taps "Check-in". The app shows the consent screen (purpose, retention period, who has access and, if the shelter is `unconfirmed`, the warning "Abrigo não confirmado pela Defesa Civil" ("Shelter not confirmed by Civil Defense") [dec. d]), with its own language selector PT | EN | ES (11.3) [dec. h, M3]. The volunteer marks the legal basis: `consent` (art. 11, I) or `vital_interest` (art. 11, II, "e") if the person is injured or incapacitated.
2. Capture with liveness (`purpose: "checkin"`, `shelter_id`).
3. `POST /v1/shelters/{id}/checkins {id, capture_id, data, legal_basis}`.
4. Server, in a transaction on `people/{person_id}`:
   - does not exist: creates with `level = verified`;
   - exists with `level = assisted`: upgrades to `verified` and keeps the history;
   - closes the open `stay` at another shelter (`left_at = now`, `exit_reason = checkin_elsewhere`) and opens the `stay` at the current shelter;
   - looks for active searches with this `person_id` (4.8) and creates matches.
5. Response: `{person_id, level, is_new, history}`. `history` includes `stays` at other shelters only if the shelter is `confirmed` [dec. d].
6. Card (3.5.1): the app offers "Entregar cartão novo" (Hand out a new card) or "Adicionar a um cartão existente" (Add to an existing card) (household that arrived together).

**Liveness not possible (injured, unconscious, small child) or offline:**
1. Photo from the app camera. The app reduces the image (6.1.4), stores the `Blob` in IndexedDB and enqueues `POST /v1/shelters/{id}/checkins {id, photo_base64, data, legal_basis}`.
2. Until it syncs, the person appears in the local list as "pendente" (pending). The card can be issued offline (3.5.1).
3. On the server, the photo goes to the `api+humantoken` flow and produces `level = assisted`. The server discards the photo after the call.
4. On receiving `2xx`, the app deletes the `Blob` [dec. a, e].
5. The person is upgraded to `verified` on the first capture with liveness [dec. i, Q1]. The shelter's "Pendentes de selfie" (Pending selfie) list shows who is `assisted`.
6. Face error (`20532`, inconclusive): the item becomes `needs_action` and the app asks for another photo. **PROPOSAL (to be confirmed) (Q28):** if there is no way to photograph again, the volunteer records "sem biometria" (no biometrics) (`level = no_biometrics`, `id` = client UUID); this record does not enter the photo search.

**Unidentified person** [dec. e]: if the check-in or rescue arrives without `name` and without `wristband`, the app also sends `thumbnail_base64` (6.1.4) and the server stores it encrypted. See 4.6.

**Child:** `data.is_child = true`. The volunteer associates already registered guardians (`POST /v1/people/{id}/guardians`), who must be `verified`. Legal basis: `child_protection` (art. 14, § 3).

### 3.5 Donations (feature 2) [dec. h, A1]

#### 3.5.1 Household card
- A `card` has a short code (`code`: 8 Crockford base32 characters, generated on the phone, readable and typeable) shown as a QR. It works as a printed card, sticker or wristband (it can be improvised with the code handwritten).
- Members: one or more people (`person_ids`) of the same household, declared at check-in. A person is on at most one active card.
- Issuance: at check-in, online or offline (`POST /v1/shelters/{id}/cards`). Loss: the volunteer cancels the card and issues another (`POST /v1/cards/{code}/cancel`).
- The card is valid at any shelter in the network.
- **PROPOSAL (to be confirmed) (Q44):** code format and the one-person-per-active-card rule.

#### 3.5.2 Day-to-day delivery
1. Admin configures the shelter's items: name, period in days and `high_value` (voucher card, mattress, household appliance) (`PUT /v1/shelters/{id}/items`). **PROPOSAL (to be confirmed):** default period 7 days (Q17); per-person rule (Q16).
2. Volunteer reads the QR with the app camera (or types the code). The app shows the card members (local cache if offline) and the volunteer taps who receives.
3. `POST /v1/shelters/{id}/deliveries {id, card_code, person_id, item, quantity, delivered_at}`.
4. Server decides whether liveness is required:
   - `high_value` item;
   - random sample: `sha256(delivery_id)` interpreted as a number in [0, 1) less than `DELIVERY_SAMPLE_RATE` (**PROPOSAL (to be confirmed)**: 0.05, Q41);
   - person with `level = assisted` (so that they are upgraded to `verified`, 4.1).
   If required: `428 liveness_required`. The app does a capture with liveness (`purpose: "revalidation"`, `expected_person_id = person_id`) and resends with `capture_id`. The capture's `person_id` different from the expected one → `409 revalidation_failed`, nothing is stored.
5. Server looks, across all active events [dec. c], for a delivery of the same `item` to the same `person_id` within the period. If there is one: `status = in_review` with `reasons: [{kind: duplicate_in_period, previous_delivery_id}]`. **The delivery is never blocked** due to duplication (history, idea 2).
6. Response: "Entrega registrada" (Delivery recorded) and, if flagged, "Enviada para revisão: outra entrega em <data>" (Sent for review: another delivery on <date>). It does not show the other delivery's shelter to an unconfirmed shelter.
7. Offline: the card works from the cache. A delivery that would require liveness (`high_value` or `assisted` person) is stored with `verification = card` and `status = in_review`, reason `liveness_skipped_offline`. The random sample does not run offline.
8. Admin reviews at `GET /v1/shelters/{id}/deliveries?status=in_review` and decides `reviewed_ok` or `irregular`, with a note.

### 3.6 Registration of rescued people (feature 3) [dec. h, A11]
Registration is done **at the landing or triage point**, not in the boat. The actor is a volunteer who is at that point.
1. Logged-in volunteer taps "Resgate" (Rescue). Photo only from the app camera.
2. The app records together: date and time, GPS of the point (`lat`, `lon`, `accuracy`), `volunteer_id`, `device_id`.
3. Optional fields: name, wristband, note, landing point name (`landing_point`), destination shelter (`destination_shelter_id`).
4. No name and no wristband: thumbnail (4.6).
5. Goes through the queue: `POST /v1/rescues`. On the server, `api+humantoken` → `person_id`; creates the person as `assisted` if they do not exist, or adds the rescue to the history. Event: that of the nearest `open` shelter within ≤ `EVENT_RADIUS_KM` of the GPS; otherwise null.
6. When the person checks in at any shelter, the token is the same and the rescue already appears in the history.
7. Matches with active searches are also looked for here.
8. Anomaly rules (4.10), with a volume threshold specific to rescue [dec. h, A11].

### 3.7 Missing person search (feature 5)
1. Precondition: volunteer of a `confirmed` shelter, or Civil Defense [dec. d].
2. The operator records: name and notes of the missing person; name, phone, relationship and language of the family; 1 to 5 photos (gallery allowed, this is the exception). **PROPOSAL (to be confirmed) (Q8):** limit of 5 photos per search and 10 searches per volunteer per day.
3. `POST /v1/searches`. **PROPOSAL (to be confirmed):** may go through the queue if offline (Q45).
4. Server generates one token per photo (`api+humantoken`), stores only the derived `person_id`s (`searches.person_ids`, "search" level: does not become a person), and compares with `people`. A match on any photo counts.
5. **The response is always the same**, with or without a match: `202 {message: "search_registered", usable_photos}`. The app shows "Busca registrada. Se a pessoa for encontrada e concordar, a equipe entrará em contato com a família." ("Search registered. If the person is found and agrees, the team will contact the family.") Not even the operator learns whether there was a match.
6. `usable_photos` says how many photos produced a token, so the operator can ask for others. It does not reveal a match.
7. **PROPOSAL (to be confirmed) (Q25):** the search stays active until the purge of the event of the originating shelter. Each later check-in, rescue or "Estou Seguro" is compared with the active searches.
8. Match → mediation (4.8).

### 3.8 "Estou Seguro" (feature 6)
1. Public page `/safe`, no login, in PT | EN | ES [dec. h, M3].
2. At the top, always visible: **"Precisa de resgate? Ligue 193 (Bombeiros) ou 199 (Defesa Civil)"** ("Need rescue? Call 193 (Fire Department) or 199 (Civil Defense)"), with two buttons `tel:193` and `tel:199`. Nothing is stored through this path [dec. h, C3].
3. To register, the person reads the notice (purpose, period, legal basis `consent`, art. 11, I) and does a capture with liveness (`purpose: "safe_check"`).
4. Chooses: "estou seguro" ("I'm safe") (`safe`) or "fui para um abrigo" ("I went to a shelter") (`went_to_shelter`, with an optional list of nearby open shelters). GPS optional.
5. **PROPOSAL (to be confirmed) (Q24):** the person checks or not "Se alguém da minha família me procurar, pode saber que estou seguro (sem dizer onde estou)" ("If someone from my family looks for me, they may know I am safe (without saying where I am)") (`family_contact_allowed`).
6. `POST /v1/safe-check`. The server creates or updates `people/{person_id}` with `level = verified` and the `safe_check` block. Looks for active searches.
7. Cost protection: 10.4.

### 3.9 Revalidation (feature 7)
Revalidation = a new capture with liveness whose resulting `person_id` is compared with the expected one (5.1).

**Donation delivery:** only `high_value` items, random sample and `assisted` person (3.5.2) [dec. h, A1].

**Child exit:**
1. Volunteer opens the child and taps "Saída" (Exit). Chooses the guardian who is picking up (from `guardian_ids`).
2. Capture with liveness of the adult (`purpose: "revalidation"`, `expected_person_id = guardian_id`).
3. `POST /v1/shelters/{id}/exits {person_id, guardian_id, capture_id}`.
4. Capture's `person_id` = `guardian_id`: exit recorded, `stay` closed.
5. Different: `409 revalidation_failed`. **PROPOSAL (to be confirmed) (Q18):** the exit is not recorded and an alert goes to the admins; an admin may record an exit by exception (`exception: {reason}`), audited and with an `exceptional_exit` alert to Civil Defense.
6. Adult exit: no revalidation, `POST /v1/shelters/{id}/exits {person_id}`.

### 3.10 Shelter confirmation [dec. d]
**By validator:** `POST /v1/shelters/{id}/confirm`. `confirmation_type = validator`, `can_endorse = true`.

**In-person endorsement:**
1. Admin of the new shelter generates a code: `POST /v1/shelters/{id}/endorsements/in-person/code {lat, lon, accuracy}` → `{code, expires_at}`, shown as a QR. **PROPOSAL (to be confirmed):** expires in 10 min (Q38).
2. Endorsing admin (of a `confirmed` shelter with `can_endorse`, with no link to the new shelter) reads the QR with the phone camera, which opens `/endorse/<code>`: `POST /v1/endorsements/in-person {code, endorser_shelter_id, lat, lon, accuracy}`.
3. Server requires both `accuracy ≤ 100` and a distance between the two phones ≤ `SHELTER_RADIUS_M`. Confirms with `confirmation_type = in_person`, `can_endorse = true`, records who endorsed.

**Remote endorsement:**
1. Admins of shelters with `can_endorse`, of the same event, see `GET /v1/events/{id}/shelters-to-endorse`: site photos, address, creator's name, distance.
2. `POST /v1/shelters/{id}/endorsements/remote {endorser_shelter_id}`.
3. With two endorsements from two different shelters, by two different volunteers, none linked to the new shelter: `confirmed`, `confirmation_type = remote`, `can_endorse = false`.
4. "Linked to the new shelter" = having or having had a `membership` with it (**PROPOSAL (to be confirmed)**, Q30).

**Revocation:** validator or Civil Defense, `POST /v1/shelters/{id}/revoke {reason}` → `unconfirmed`, `can_endorse = false`; each volunteer who gave an endorsement becomes `flagged` and generates an `endorser_flagged` alert. No cascade (**PROPOSAL (to be confirmed)**, Q20).

### 3.11 Civil Defense dashboard
`GET /v1/dashboard/events` and `GET /v1/dashboard/events/{id}`: per event, shelters (name, confirmation, `flagged`, position), unique people per shelter (`current_shelter_id`), total unique people in the event, movements between shelters (`stays` closed by `checkin_elsewhere` in the last 24 h and 7 days), rescues, open alerts, matches in the Civil Defense queue, shelters awaiting endorsement. Counts via Firestore `count()` aggregation.

---

## 4. Business rules

### 4.1 Trust levels of people

| `level` | Origin | Unlocks | Transitions |
|---|---|---|---|
| `verified` | Capture with liveness (check-in, revalidated delivery, "Estou Seguro") | Shelter, donation, being a child's guardian | final |
| `assisted` | Photo from the app camera (rescue, offline check-in or without liveness) | Location and reunification; donation only with capture with liveness (which raises the level) or in review if offline | → `verified` on the first capture with liveness, with no new registration [dec. i, Q1] |
| `no_biometrics` | Manual record without token (PROPOSAL, Q28) | Presence at the shelter | none |
| "search" | Family photo | Nothing; stays only in `searches.person_ids` | does not become a person |

Level never goes down.

### 4.2 Shelter
- `status`: `open` → `closed` (admin or Civil Defense); `open` → `merged` (merge accepted).
- `confirmation`: 2.2 and 3.10.
- `unconfirmed` sees only what it registered itself; people registered in it enter the network search normally [dec. d].
- Minimum radius between open shelters: `SHELTER_RADIUS_M` (default 100) [dec. a].
- Required accuracy: `accuracy ≤ 100 m` on creation and on in-person endorsements.

### 4.3 Automatic events [dec. c]
On creation of a shelter online (or on sync of one created offline):
1. If the request carried `declared_event_ids`, it joins the first declared event that is `active` (`event_link = declared`). **PROPOSAL (to be confirmed) (Q31):** it takes part in the chain rule like any other.
2. Otherwise, it looks for `open` shelters of `active` events within ≤ `EVENT_RADIUS_KM` (default 50):
   - none: creates an event with the automatic name "Cidade/UF – mês/ano" (City/State – month/year); with no address yet, "Evento – mês/ano" (Event – month/year), redone when the address arrives if `auto_named` is still `true`;
   - one event: joins it;
   - two or more: **event merge**. The oldest survives; the others become `status = merged`, `merged_into = <survivor>`; shelters, searches, rescues and `people.event_ids` are repointed in batches.
3. An admin of any shelter of the event renames it (`PATCH /v1/events/{id}`) → `auto_named = false`.
4. Two nearby disasters become a single event [dec. c].
5. Activity: any write to a shelter of the event updates `events.last_activity_at`, at most once per hour (in-memory cache).
6. Automatic closing (hourly job, 6.2.6): an `active` event with all shelters `closed`/`merged`, or with `now − last_activity_at ≥ INACTIVITY_DAYS` (default 30) → `closed`, `closed_at = now`, `purge_at = now + PURGE_AFTER_DAYS`.
7. A `closed` event does not reopen. **PROPOSAL (to be confirmed) (Q36):** a shelter of a closed event does not accept writes (`409 event_closed`).
8. The event organizes but does not isolate: search and donation duplication look at all active events.

### 4.4 Invites and members
Rules in 3.3 and 2.3. Removal applies only to that shelter and generates a `volunteer_removed` alert for the admins of the other shelters in which the person is an active member [dec. b]. Ban (Civil Defense): `volunteers.banned = true`, sessions deleted, all `memberships` `removed`.

### 4.5 100 m radius
- Distance by haversine over `lat/lon`; candidates by geohash (6.3.3).
- Used in: shelter creation and in-person endorsement.

### 4.6 Thumbnail of an unidentified person [dec. e]
- Generated on the phone: 128×128 px, JPEG quality 0.6 (~5–10 KB).
- Sent along with the check-in or rescue when there is neither `name` nor `wristband`.
- On the server: AES-256-GCM (`THUMBNAIL_KEY`, random 12-byte nonce), field `people.thumbnail_encrypted`.
- Never used to generate a token.
- Who sees it (`GET /v1/people/{id}/thumbnail`): active members of the `current_shelter_id`, the mediator of an open match about the person and — **PROPOSAL (to be confirmed) (Q33)** — if the person does not have a shelter yet, the landing point volunteer who recorded the rescue and Civil Defense. Each access writes an audit record.
- Deleted in the same write that fills in `name` or `wristband`, or when all of the person's events are closed. Whichever comes first.

### 4.7 Donations
- Duplication key: `(person_id, item)`. Window: `items[item].period_days` of the delivering shelter. Searches across all active events.
- Duplication → `in_review`, never blocking.
- Liveness on delivery only for `high_value`, random sample and `assisted` person [dec. h, A1].
- `verification`: `card` (default), `liveness` (revalidated). Offline delivery that would require liveness → `in_review`, reason `liveness_skipped_offline`.

### 4.8 Search mediation
**Never return location to whoever searched.** When creating a match (`matches/{sha256(search_id|person_id)}`, idempotent), the server chooses the mediator:

| Situation of the person found | `mediator_kind` |
|---|---|
| `current_shelter_id` `confirmed` | `shelter` (admins of that shelter) |
| `current_shelter_id` `unconfirmed` | `civil_defense` [dec. d] (**PROPOSAL**: not a nearby confirmed shelter, Q24) |
| No shelter (rescue only) | `civil_defense` |
| "Estou Seguro" only, with `family_contact_allowed` | `search_origin`: admins of the shelter that registered the search, who receive only "declarou-se segura em <data>" ("declared themselves safe on <date>") and the family's contact (PROPOSAL, Q24) |
| "Estou Seguro" only, without prior consent | `none`; match stored as `declined_in_advance` |

Mediator flow:
1. Sees the notice in "Avisos" (Notices) (`GET /v1/matches`): name of the missing person according to the family, name of the person in the registry, thumbnail if any, shelter. **Does not see the family's contact yet.**
2. Talks to the person. The person decides; if incapacitated, the mediator decides (`decided_by = operator`).
3. `POST /v1/matches/{id}/decision {decision: accept|decline, decided_by: person|operator, note}`.
4. `accept`: the response carries `family {name, phone, relationship, language}` and `whatsapp_link` (`https://wa.me/<phone>?text=<mensagem>`, in the family's language). Location is never sent automatically.
5. `decline`: nothing is sent; the search remains active for other people, with no new match for this `person_id`.
6. Every decision writes an audit record with `legal_basis = vital_interest`.

### 4.9 Child
- `is_child = true` requires `verified` `guardian_ids` for exit with revalidation.
- `legal_basis = child_protection` (art. 14, § 3).

### 4.10 Audit and anomalies
Every write and every access to a thumbnail or to match data writes `audit_log`. Anomalies generate an alert, without blocking. **PROPOSAL (to be confirmed) (Q39)** for the thresholds:
- `anomaly_volume`: check-ins by the same volunteer above `ANOMALY_CHECKINS_PER_10MIN` (default 30); rescues above `ANOMALY_RESCUES_PER_10MIN` (default 100, because many records in a row are normal at the landing point [dec. h, A11]).
- `anomaly_distance`: the same `person_id` recorded at two points more than 50 km apart less than 1 h apart.
- `out_of_area`: rescue more than `EVENT_RADIUS_KM` from all shelters where the volunteer is a member.
Alerts go to Civil Defense and to the admins of the volunteer's shelters.

---

## 5. Integration with Unico

### 5.1 Flows used

| Screen | Flow | Level |
|---|---|---|
| Login, invite, online check-in, revalidation (delivery and child exit), "Estou Seguro" | `humantoken` (Journey, with liveness) | `verified` |
| Check-in without liveness or offline, rescue | `api+humantoken` (without liveness) | `assisted` |
| Family search | `api+humantoken` | search |
| — | `api+liveness+humantoken*`: **not used**; rejects ordinary JPEG with `400 INVALID_CAPTURE_TOKEN` (test of 2026-10-20) | — |
| — | `humantoken+revalidation`, `api+liveness+humantoken+revalidation`: **not used** (13) | — |

**Revalidation without the revalidation flow.** A new `humantoken` journey and comparison of the derived `person_id`. Same result and cost as the `+revalidation` flow, without storing the raw token (6.3.2).

**Journey instead of SDK** [dec. i, Q4]: liveness through Unico's journey (page opened by the phone). **First implementation task:** test the journey from the PWA installed on iPhone (navigation outside the app scope in standalone mode, camera permission, return via `callbackUri`). Plan B: Unico's web SDK.

### 5.2 Journey (`humantoken`)
1. `POST /v1/captures` → server calls `POST {UNICO_URL}/process` with `{"flow": "humantoken", "callbackUri": "https://<domain>/return?capture=<capture_id>"}`. Stores `process_id = id` and returns `link`. Capture expires in 15 min.
2. The endpoint that consumes the capture calls `GET {UNICO_URL}/process/{process_id}` if it has not been resolved yet (polling does not create a process, costs nothing).
3. States:
   - `PROCESS_STATE_CREATED`: `202 capture_pending`; the app retries at 2 s, 4 s, 8 s, up to 2 min.
   - `PROCESS_STATE_FINISHED` with **non-empty** `data.humanToken.value`: success.
   - `PROCESS_STATE_FINISHED` without `value` or with empty `value`: `422 capture_inconclusive`.
   - `PROCESS_STATE_FAILED`: `422 capture_failed`.
   - `PROCESS_STATE_CANCELED`: `422 capture_canceled`.
   - others (`UNSPECIFIED`, `AWAITING_FOR_DOCUMENT`): `502 unico_unexpected_state`, with a log.
4. The capture is single-use (`consumed = true` in the same transaction that stores the result) and can only be consumed by the `device_id` that created it.

### 5.3 API Only (`api+humantoken`)
- Body: `{"flow": "api+humantoken", "imageBase64": "<base64>"}`. PNG/JPEG/WebP ≤ 800 KB; above that the server responds `413 image_too_large` without calling Unico.
- Success: non-empty `data.humanToken.value`.
- No face (`20532`): `422 no_face`. Empty `value` or `PROCESS_STATE_FAILED`: `422 capture_inconclusive`.
- `400 INVALID_IMAGE`, `UNSUPPORTED_IMAGE_FORMAT`, `IMAGE_TOO_LARGE`, `MISSING_IMAGE`: `422 invalid_image`.

### 5.4 Empty `value` rule
A single function in the Unico client (`extract_token`) returns `Option<String>` and treats missing, empty string and whitespace-only as "no token". No other code reads `data.humanToken.value`.

### 5.5 Errors, 429 and backoff
- `429` with message `monthly process limit ... reached`: do not retry. `503 unico_limit`, `unico_limit` alert for Civil Defense, `error` log.
- `429` without that message: exponential backoff with jitter on the server (1, 2, 4, 8 s + 0–1 s random), at most 4 attempts. After that, `503 unico_busy` with `Retry-After: 30`; the app queue reschedules (9.2).
- `500`/timeout (60 s): one new attempt after 2 s; then `502 unico_unavailable`.
- `401`/`403 PERMISSION_DENIED`: `500 unico_misconfigured` and `error` log.
- Concurrency: semaphore of 4 simultaneous calls to Unico in the process.
- There is no sandbox: every call is production. Automated tests use a fake server (`UNICO_URL`); real calls only in manual testing.

### 5.6 Key
`HUMAN_TOKEN_ACCESS_KEY` (format `uk_live_...`) only in the VM environment file (6.4) or in the local `.env`. It never goes to the phone [dec. a, g]. Rotation through the Unico API invalidates the old one immediately: update the vault, run `configure.yml` and restart the service.

---

## 6. Architecture and deployment

```
Phone (TypeScript PWA) ──HTTPS──► Caddy (e2-micro VM)
   │  IndexedDB + queue                ├── /           → PWA static files
   │                                   └── /v1/*       → Rust API (127.0.0.1:8080)
   ├──► Nominatim (address only)                        ├──► Firestore (same region as the VM)
   └──► Unico Journey (capture with liveness only)      └──► Unico Human Token API
```
The phone never talks to Firestore or to the Unico API; it only opens the journey page [dec. g].

**Region per installation** [dec. h, A8]: the GCP region is configuration of each installation (`gcp_region`/`gcp_zone` in the Ansible inventory). The pilot runs in `us-east1` (zone `us-east1-b`), which has the e2-micro free tier and is closer to Brazil than `us-central1` [dec. g]. A city government can bring up its own instance in `southamerica-east1` (São Paulo), outside the free tier; the data protection impact assessment (RIPD) documents this (10.1.4). Pilot domain: `taba.thingsbuilder.com`.

### 6.1 PWA
- **Stack:** Vite + TypeScript + Preact + `idb` + `vite-plugin-pwa` (Workbox, cached app shell). Routing by `location.pathname` with a simple `switch`.
- **QR:** generation with `qrcode`; reading with `BarcodeDetector` when available and `jsQR` as a fallback (Safari on iOS does not have `BarcodeDetector`).
- **Routes:** `/`, `/login`, `/return`, `/shelters/new`, `/shelter/:id`, `/shelter/:id/checkin`, `/shelter/:id/people`, `/shelter/:id/delivery`, `/shelter/:id/exit`, `/shelter/:id/members`, `/person/:id`, `/rescue`, `/search`, `/inbox`, `/invite/:code`, `/safe`, `/dashboard`, `/endorse/:code`, `/privacy`.

#### 6.1.1 IndexedDB (`taba`, version 1)
| Store | Key | Content |
|---|---|---|
| `config` | name | `device_id`, `language`, `session` (secret + `volunteer_id` + `expires_at`) |
| `queue` | `id` (UUID) | `{method, path, body, volunteer_id, shelter_id?, created_at, attempts, next_attempt_at, state: pending\|held\|needs_action, error?}` |
| `blobs` | `queue_id` | Pending photos (`Blob`), deleted when the item receives `2xx` |
| `shelters` | `id` | The volunteer's shelters (cache) |
| `people` | `id` | People of the current shelter (offline cache by name/wristband), including local pending ones |
| `cards` | `code` | Cards of the current shelter and their members (offline cache) |
| `captures` | `capture_id` | Capture in progress (to resume after returning from the journey) |
| `addresses` | `shelter_id` | Coordinates awaiting Nominatim |

#### 6.1.2 Persistence
`navigator.storage.persist()` on the first logged-in use; result shown in "Configurações" (Settings).

#### 6.1.3 Camera
`getUserMedia({video: {facingMode: "user" | "environment"}})`, frame in `canvas`. No `<input type="file">` in check-in and rescue. In search, `<input type="file" accept="image/*" multiple>`.

#### 6.1.4 Image reduction
`canvas` → `toBlob("image/jpeg", q)`:
- Photo for token: longest side 1280 px; quality starts at 0.85 and drops by 0.1 until ≤ 300 KB (**PROPOSAL (to be confirmed)**, Q34). Egress traffic above 1 GB/month is a known cost of a real event (~US$ 0.12/GB) [dec. i, Q5].
- Thumbnail: 128×128, quality 0.6.
- Site photo: longest side 1024 px, ≤ 200 KB.

#### 6.1.5 Geolocation
`getCurrentPosition` with `enableHighAccuracy: true`; always stores `accuracy`.

#### 6.1.6 Nominatim
`GET https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=..&lon=..&accept-language=pt-BR`, at most 1 request per second (Nominatim policy), result stored in IndexedDB. No signal: stores in `addresses` and tries when it comes back; once obtained, `PATCH /v1/shelters/{id} {address}` through the queue.

### 6.2 Rust API
A single `taba` binary with two subcommands: `serve` (API + hourly job) and `role` (global roles CLI).

**Crates:** `axum`, `tokio`, `tower-http` (3 MB body limit, trace), `serde`/`serde_json`, `reqwest` (rustls), `firestore` (gRPC; authentication through the VM's service account via the metadata server; emulator via `FIRESTORE_EMULATOR_HOST` in tests), `geohash`, `hmac` + `sha2`, `aes-gcm`, `rand`, `base64`, `uuid`, `time`, `governor` (per-IP limit), `tracing` + `tracing-subscriber`, `anyhow`.

#### 6.2.1 Modules
`main.rs` (CLI and router), `config.rs`, `auth.rs` (sessions, volunteer extractor, role check), `unico.rs`, `geo.rs` (geohash and haversine), `shelters.rs`, `events.rs`, `members.rs` (invites and memberships), `people.rs` (check-in, exits, thumbnail), `cards.rs`, `deliveries.rs`, `rescues.rs`, `searches.rs` (searches and matches), `safe_check.rs`, `dashboard.rs`, `jobs.rs` (closing and purge), `audit.rs` (audit and anomalies), `idempotency.rs`.

#### 6.2.2 Configuration (environment variables)
On the VM, read from `/etc/taba/env`; in development, from `.env` created from `.env.example` (6.4).

| Variable | Default | Use |
|---|---|---|
| `HUMAN_TOKEN_ACCESS_KEY` | — | Unico key |
| `UNICO_URL` | `https://humantoken.unico.app/client/v1` | Replaceable in tests |
| `DOMAIN` | — | Builds `callbackUri` and links |
| `GCP_PROJECT` | — | Firestore (already generated by the current `env.j2`) |
| `HMAC_KEY` | — | 32 bytes base64; derives `person_id`/`volunteer_id` [dec. i, Q3] |
| `THUMBNAIL_KEY` | — | 32 bytes base64; AES-GCM |
| `SHELTER_RADIUS_M` | 100 | [dec. a] |
| `EVENT_RADIUS_KM` | 50 | [dec. c] |
| `INACTIVITY_DAYS` | 30 | [dec. c] |
| `PURGE_AFTER_DAYS` | 30 | **PROPOSAL (to be confirmed)**, Q9 |
| `SESSION_DAYS` | 7 | **PROPOSAL (to be confirmed)**, Q26 |
| `PUBLIC_CAPTURES_PER_DAY` | 500 | **PROPOSAL (to be confirmed)**, Q15 |
| `DELIVERY_SAMPLE_RATE` | 0.05 | **PROPOSAL (to be confirmed)**, Q41 |
| `ANOMALY_CHECKINS_PER_10MIN` | 30 | **PROPOSAL (to be confirmed)**, Q39 |
| `ANOMALY_RESCUES_PER_10MIN` | 100 | **PROPOSAL (to be confirmed)**, Q39 |

#### 6.2.3 Error format
`{"error": "<stable_code>", "detail": {...}}`. The app translates the code; the server does not translate text. Log messages in English [dec. j].

#### 6.2.4 Concurrency and uniqueness
- **Person and volunteer uniqueness:** by the document ID (6.3.2) in a `get` → `create`/`update` transaction [dec. g].
- **Shelter creation and event merge:** a global `tokio::sync::Mutex` in the process. `// ponytail: global lock; fine with the single e2-micro instance. With more than one instance, switch to a transaction on a per-geohash-cell lock document.`
- **Matches:** deterministic ID (`sha256(search_id|person_id)`) and a `create` that fails if it already exists.
- **Cards:** `cards/{code}` created with `create` (fails if the code already exists; the app generates another). One person per active card: checked in the same transaction.

#### 6.2.5 Authentication
- Session: secret of 32 random bytes (base64url) returned to the app; in Firestore, `sessions/{sha256(secret)}`. Header `Authorization: Bearer <secret>`.
- Expires in `SESSION_DAYS`, renewed on each authenticated request when more than 1 day has passed since the last renewal.
- A ban deletes the volunteer's sessions.
- The `Volunteer` extractor loads the profile and active `memberships`; each handler checks role and shelter state according to the matrix in 2.3.

#### 6.2.6 Hourly job (closing and purge)
`tokio` task inside `serve`, every hour:
1. Closes events (4.3.6).
2. Deletes thumbnails of people whose events are all closed.
3. Purges events with `purge_at ≤ now` (10.1.2), in batches of 400 writes, resuming in the next hour if it exceeds 15,000 deletions in the day (free tier: 20,000/day).
4. Expires overdue captures, invites and endorsement codes (those that do not use TTL).

### 6.3 Firestore
The database is `(default)`, in native mode, in the installation's region — the only one that uses the free quota (`provision.yml`).

#### 6.3.1 Collections — section 7.

#### 6.3.2 Identifiers derived from the token [dec. i, Q3]
`person_id = volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))` (43 characters). The raw token is not stored anywhere. Uniqueness is still by the document ID [dec. g]. Reasons: the token format is not documented and may contain `/` (forbidden in a Firestore ID); the token is the same in any Unico client, and the HMAC prevents cross-referencing databases in case of a leak; revalidation compares derived IDs.
`HMAC_KEY` is a secret of its own, in each installation's vault, with a backup outside the VM: losing it makes all IDs incomparable with new tokens.

#### 6.3.3 Geohash
- Field `geohash` (precision 9) in `shelters`.
- "Within ≤ r meters" query: choose the highest precision `p` whose cell has its shortest side ≥ r (p = 7 for 100 m; p = 3 for 50 km), compute the point's cell and its 8 neighbors, and run 9 range queries (`geohash >= prefix` and `< prefix + "~"`) with `status == "open"`. Filter by haversine.

#### 6.3.4 Composite indexes (`infra/firestore/firestore.indexes.json`)
| Collection | Fields |
|---|---|
| `shelters` | `status` ASC, `geohash` ASC |
| `shelters` | `event_id` ASC, `status` ASC |
| `people` | `current_shelter_id` ASC, `updated_at` DESC |
| `cards` | `shelter_id` ASC, `status` ASC |
| `cards` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `deliveries` | `person_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `shelter_id` ASC, `status` ASC, `delivered_at` DESC |
| `searches` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `matches` | `mediator_kind` ASC, `mediator_shelter_id` ASC, `status` ASC, `created_at` DESC |
| `memberships` | `volunteer_id` ASC, `status` ASC |
| `memberships` | `shelter_id` ASC, `status` ASC |
| `alerts` | `resolved` ASC, `created_at` DESC |
| `audit_log` | `shelter_id` ASC, `at` DESC |
| `audit_log` | `volunteer_id` ASC, `at` DESC |

TTL policies: `sessions.expires_at`, `captures.expires_at`, `idempotency.expires_at`, `audit_log.expires_at`.

#### 6.3.5 Free tier quotas (reference)
1 GiB stored; 50,000 reads, 20,000 writes and 20,000 deletes per day. Each operation = 1 business write + 1 audit write.

### 6.4 VM and deployment
The infrastructure is created and configured with Ansible in `infra/ansible/`: `provision.yml` (APIs, Firestore, service account with only `roles/datastore.user`, static IP, firewall 22/80/443, e2-micro VM with Debian 13 and a 30 GB `pd-standard` disk) and `configure.yml` (automatic updates, Caddy, `/etc/taba/env`). The step-by-step guide is in **`infra/README.md`**; this spec does not repeat it.

- **Inventory:** `infra/ansible/inventory/example/` (in git, example values) and `inventory/prod/` (outside git, each installation's values and vault). No installation value (project, IP, keys) enters the repository.
- **Secrets:** on the VM, `/etc/taba/env` (mode 0600, directory 0700) generated by the `env.j2` template from the installation's vault. The API deployment adds to `env.j2` and to `vault.yml.example` the missing variables from 6.2.2 (`HMAC_KEY`, `THUMBNAIL_KEY`, `DOMAIN`, limits), with vault keys `vault_taba_hmac_key` and `vault_taba_thumbnail_key`. In development, `.env` from `.env.example` (which will list all variables, without values).
- **Caddy:** currently responds only with a placeholder (`Caddyfile.j2`). The API deployment replaces it with: `file_server` for the PWA at `/srv/taba/pwa` and `reverse_proxy /v1/* 127.0.0.1:8080`, with the headers `Strict-Transport-Security`, `Content-Security-Policy` (`default-src 'self'; connect-src 'self' https://nominatim.openstreetmap.org; img-src 'self' blob: data:`), `X-Content-Type-Options`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(self), geolocation=(self)`.
- **systemd:** `taba.service` (`ExecStart=/usr/local/bin/taba serve`, `EnvironmentFile=/etc/taba/env`, `Restart=always`, `DynamicUser=yes`), installed by a new task in `configure.yml`.
- **Swap:** 1 GB (the VM has 1 GB of RAM), also through `configure.yml`.
- **Build:** the binary is compiled outside the VM (`x86_64-unknown-linux-gnu`) and copied; the e2-micro does not have enough memory to compile Rust.
- The API trusts `X-Forwarded-For` only when coming from `127.0.0.1`.
- Cost: free tier, except the external IPv4 (~US$ 3/month) [dec. g] and egress above 1 GB/month [dec. i, Q5].

---

## 7. Data model (Firestore)

Dates: `Timestamp`. `client_*` = phone time; no prefix = server time.

### `events/{event_id}` (server UUID)
`name`, `auto_named: bool`, `status: active|closed|merged|purged`, `merged_into?`, `created_at`, `last_activity_at`, `closed_at?`, `closure_reason?: all_shelters_closed|inactivity`, `purge_at?`, `purged_at?`, `stats?: {shelters, unique_people, rescues, deliveries}` (stored at purge).

### `shelters/{shelter_id}` (client UUID)
`name`, `lat`, `lon`, `accuracy_m`, `geohash`, `address: {text, city, state, pending: bool}`, `event_id?`, `event_link: auto|declared`, `created_by`, `client_created_at`, `created_at`, `status: open|closed|merged`, `merged_into?`, `confirmation: unconfirmed|confirmed`, `confirmation_type?: validator|in_person|remote`, `can_endorse: bool`, `endorsements: [{kind: in_person|remote, volunteer_id, endorser_shelter_id?, at, distance_m?}]`, `revocations: [{by, at, reason}]`, `flagged: bool`, `other_site_reason?`, `conflicts_with?`, `items: {<name>: {period_days, high_value: bool}}`, `photo_count`.

Subcollection `shelters/{id}/photos/{n}`: `image: bytes`, `content_type`, `uploaded_by`, `uploaded_at`.

### `volunteers/{volunteer_id}` (HMAC of the token)
`name`, `phone?`, `show_phone: bool`, `language: pt-BR|en`, `global_roles: [validator|civil_defense]`, `banned: bool`, `banned_by?`, `banned_at?`, `flagged: bool`, `flag_reasons: []`, `created_at`, `last_seen_at`.

### `memberships/{shelter_id}_{volunteer_id}`
`shelter_id`, `volunteer_id`, `role: admin|volunteer`, `status: active|removed|left`, `via: creator|qr|link|merge`, `invite_code?`, `invited_by?`, `joined_at`, `removed_by?`, `removed_at?`.

### `invites/{code}` (128 random bits, base64url)
`shelter_id`, `kind: qr|link`, `role: admin|volunteer` (`link` ⇒ `volunteer`), `created_by`, `created_at`, `expires_at`, `status: active|used|canceled`, `used_by?`, `used_at?`.

### `endorsement_codes/{code}`
`shelter_id`, `created_by`, `lat`, `lon`, `accuracy_m`, `expires_at`, `used: bool`.

### `sessions/{sha256(secret)}`
`volunteer_id`, `device_id`, `created_at`, `renewed_at`, `expires_at` (TTL).

### `captures/{capture_id}` (server UUID)
`process_id` (Unico), `purpose: login|invite|checkin|revalidation|safe_check`, `device_id`, `volunteer_id?`, `shelter_id?`, `expected_person_id?`, `status: created|ok|inconclusive|failed|canceled`, `person_id?` (HMAC; never the token), `consumed: bool`, `created_at`, `expires_at` (15 min, TTL).

### `people/{person_id}` (HMAC of the token; client UUID if `no_biometrics`)
`level: verified|assisted|no_biometrics`, `name?`, `wristband?`, `is_child: bool`, `birth_year?`, `needs?` (free text), `guardian_ids: []`, `card_code?`, `current_shelter_id?`, `stays: [{shelter_id, event_id, entered_at, left_at?, exit_reason?: checkin_elsewhere|exit|exceptional_exit, registered_by}]`, `event_ids: []`, `rescue_ids: []`, `thumbnail_encrypted?: bytes`, `safe_check?: {status: safe|went_to_shelter, reported_shelter_id?, lat?, lon?, family_contact_allowed: bool, at}`, `origin: checkin|rescue|safe_check`, `legal_basis`, `created_at`, `updated_at`, `client_edited_at`.

### `cards/{code}` (8 Crockford base32 characters, generated on the client)
`shelter_id` (issuer), `person_ids: []`, `status: active|canceled`, `issued_by`, `client_issued_at`, `created_at`, `canceled_at?`, `replaced_by?`.

### `deliveries/{delivery_id}` (client UUID)
`shelter_id`, `event_id`, `person_id`, `card_code?`, `item`, `quantity`, `delivered_by`, `delivered_at` (client), `received_at`, `verification: card|liveness`, `status: ok|in_review|reviewed_ok|irregular`, `reasons: [{kind: duplicate_in_period|liveness_skipped_offline, previous_delivery_id?}]`, `reviewed_by?`, `review_note?`.

### `rescues/{rescue_id}` (client UUID)
`person_id`, `volunteer_id`, `device_id`, `lat`, `lon`, `accuracy_m`, `landing_point?`, `captured_at` (client), `received_at`, `event_id?`, `destination_shelter_id?`, `note?`.

### `searches/{search_id}` (client UUID)
`created_by`, `origin_shelter_id?`, `event_id?`, `person_ids: []` (HMAC of the photos' tokens), `photos_sent`, `usable_photos`, `missing_person: {name, notes}`, `family: {name, phone, relationship, language}`, `status: active|closed`, `created_at`.

### `matches/{sha256(search_id|person_id)}`
`search_id`, `person_id`, `mediator_kind: shelter|civil_defense|search_origin|none`, `mediator_shelter_id?`, `status: open|accepted|declined|declined_in_advance`, `decided_by?: person|operator`, `decided_by_volunteer?`, `decided_at?`, `note?`, `created_at`.

### `merges/{merge_id}`
`source_shelter_id`, `target_shelter_id`, `requested_by`, `status: pending|accepted|declined`, `decided_by?`, `created_at`, `decided_at?`.

### `alerts/{alert_id}`
`kind: anomaly_volume|anomaly_distance|out_of_area|volunteer_removed|shelter_flagged|unico_limit|endorser_flagged|exceptional_exit`, `target_kind`, `target_id`, `event_id?`, `shelter_ids: []`, `for_civil_defense: bool`, `detail`, `created_at`, `resolved: bool`, `resolved_by?`.

### `audit_log/{auto}`
`at`, `volunteer_id?`, `device_id?`, `ip?`, `shelter_id?`, `action`, `target_kind`, `target_id`, `legal_basis?`, `lat?`, `lon?`, `expires_at` (TTL; **PROPOSAL**: 1 year, Q21). No name and no photo.

### `idempotency/{key}`
`volunteer_id?`, `route`, `http_status`, `body`, `expires_at` (7 days, TTL).

`legal_basis` (enum): `consent` (art. 11, I) | `vital_interest` (art. 11, II, "e") | `fraud_prevention` (art. 11, II, "g") | `public_policy` (art. 11, II, "b") | `child_protection` (art. 14, § 3) | `family_consent` (art. 7, I).

---

## 8. REST API

Base `https://<domain>/v1`. JSON. Authentication `Authorization: Bearer <session>` except where marked "public". Routes that write accept `Idempotency-Key` (the app uses the queue item's `id`) and `X-Device-Id`; a repeated key returns the response stored in `idempotency`.

### 8.1 Captures and session
| Method and route | Auth | Body | Response |
|---|---|---|---|
| `POST /captures` | public for `login`, `invite`, `safe_check`; session for the others | `{purpose, shelter_id?, expected_person_id?}` | `201 {capture_id, link, expires_at}`; `429 ip_limit`; `503 public_daily_limit` |
| `POST /sessions` | public | `{capture_id, profile?: {name, phone?, show_phone, language}}` | `201 {session, expires_at, volunteer}`; `202 capture_pending`; `422 profile_required`; `403 banned` |
| `DELETE /sessions/current` | session | — | `204` |
| `GET /me` | session | — | `{volunteer, memberships: [{shelter_id, name, role, confirmation}]}` |
| `PATCH /me` | session | `{name?, phone?, show_phone?, language?}` | `200` |

### 8.2 Shelters and events
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters` | `{id, name, lat, lon, accuracy, address?, other_site_reason?, declared_event_ids?: [], site_photos?: [base64], client_created_at}` | `201 {shelter, event?}`; `409 shelter_nearby {shelter_id, name, distance_m, contact}`; `422 insufficient_accuracy` |
| `GET /shelters/{id}` | — | shelter (active members; other volunteers see only name and confirmation) |
| `PATCH /shelters/{id}` | `{name?, address?}` (admin) | `200` |
| `POST /shelters/{id}/close` | — (admin or DC) | `200` |
| `PUT /shelters/{id}/items` | `{items: {<name>: {period_days, high_value}}}` (admin) | `200` |
| `POST /shelters/{id}/photos` | `{image_base64}` (admin, max. 2) | `201` |
| `GET /shelters/{id}/photos/{n}` | — (shelter admin, eligible endorser, Val, DC) | `image/jpeg` |
| `GET /events/nearby?lat&lon` | — | `[{event_id, name}]` |
| `PATCH /events/{id}` | `{name}` | `200` |
| `POST /shelters/{id}/merges` | `{target_shelter_id}` (source admin) | `201 {merge_id}` |
| `POST /merges/{id}/decision` | `{decision: accept|decline}` (destination admin, `confirmed` shelter) | `200` |

### 8.3 Confirmation
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/confirm` | — (Val, DC) | `200` |
| `POST /shelters/{id}/revoke` | `{reason}` (Val, DC) | `200` |
| `POST /shelters/{id}/endorsements/in-person/code` | `{lat, lon, accuracy}` (admin of the new shelter) | `201 {code, expires_at}` |
| `POST /endorsements/in-person` | `{code, endorser_shelter_id, lat, lon, accuracy}` | `200 {shelter}`; `403 cannot_endorse`; `422 too_far {distance_m}` |
| `GET /events/{id}/shelters-to-endorse?endorser_shelter_id` | — | `[{shelter_id, name, address, creator_name, distance_m, photo_count}]` |
| `POST /shelters/{id}/endorsements/remote` | `{endorser_shelter_id}` | `200 {endorsements, confirmed: bool}` |

### 8.4 Invites and members
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/invites` | `{kind: qr|link, role}` (admin) | `201 {code, url, expires_at}`; `422 link_volunteer_only` |
| `GET /shelters/{id}/invites` | — (admin) | active ones and who joined via link |
| `DELETE /invites/{code}` | — (admin) | `204` |
| `GET /invites/{code}` | public | `{shelter_name, role, confirmed, valid}` |
| `POST /invites/{code}/accept` | public `{capture_id, profile?}` | `201 {session, membership}`; `410 invite_invalid`; `409 already_member`; `403 banned\|removed_from_shelter` |
| `GET /shelters/{id}/members` | — (member) | list |
| `PATCH /shelters/{id}/members/{volunteer_id}` | `{role: admin}` (admin) | `200` |
| `DELETE /shelters/{id}/members/{volunteer_id}` | — (admin removes volunteer; DC removes anyone) | `204`; `403 admin_cannot_remove_admin`; `409 last_admin` |
| `POST /shelters/{id}/leave` | — | `204`; `409 last_admin` |
| `POST /volunteers/{volunteer_id}/ban` | `{reason}` (DC) | `200` |

### 8.5 People and cards
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/checkins` | `{id, capture_id? \| photo_base64? \| no_biometrics: true, data: {name?, wristband?, is_child, birth_year?, needs?}, thumbnail_base64?, legal_basis: consent\|vital_interest\|child_protection, client_created_at}` | `201 {person_id, level, is_new, history}`; `422 no_face\|capture_inconclusive\|invalid_image` |
| `GET /shelters/{id}/people` | — (member) | people with `current_shelter_id = id` (+ filtered `stays`) |
| `GET /shelters/{id}/people/needs-selfie` | — | `assisted` at the shelter |
| `GET /people/{person_id}` | — (member of the `current_shelter_id` or of a shelter where the person stayed) | person with `stays` filtered per 2.3 |
| `PATCH /people/{person_id}` | `{data, client_edited_at}` | `200`; deletes the thumbnail if `name` or `wristband` come filled in |
| `POST /people/{person_id}/guardians` | `{guardian_id}` | `200`; `422 guardian_not_verified` |
| `GET /people/{person_id}/thumbnail` | — (4.6) | `image/jpeg`, audited |
| `POST /shelters/{id}/exits` | `{person_id, guardian_id?, capture_id?, exception?: {reason}}` | `200`; `409 revalidation_failed` |
| `POST /shelters/{id}/cards` | `{code, person_ids: [], client_issued_at}` | `201`; `409 code_taken\|person_has_card` |
| `GET /cards/{code}` | — (member of any shelter) | `{code, status, members: [{person_id, name, level}]}` (no history) |
| `PATCH /cards/{code}` | `{add_person_ids?, remove_person_ids?}` | `200` |
| `POST /cards/{code}/cancel` | `{replaced_by?}` | `200` |

### 8.6 Deliveries
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/deliveries` | `{id, card_code, person_id, item, quantity, delivered_at, capture_id?}` | `201 {delivery_id, status, reasons}`; `428 liveness_required`; `409 revalidation_failed`; `404 card_not_found\|person_not_on_card` |
| `GET /shelters/{id}/deliveries?status=` | — (admin) | list |
| `POST /deliveries/{id}/review` | `{decision: reviewed_ok\|irregular, note}` (admin) | `200` |

### 8.7 Rescues, searches, matches, "Estou Seguro"
| Method and route | Body | Response |
|---|---|---|
| `POST /rescues` | `{id, photo_base64, lat, lon, accuracy, captured_at, data?: {name?, wristband?, note?}, landing_point?, destination_shelter_id?, thumbnail_base64?}` | `201 {rescue_id, person_id, level}` |
| `PATCH /rescues/{id}` | `{landing_point?, destination_shelter_id?}` (author) | `200` |
| `POST /searches` | `{id, photos_base64: [1..5], missing_person: {name, notes}, family: {name, phone, relationship, language}}` | `202 {message: "search_registered", usable_photos}` (same with or without a match) |
| `GET /matches` | — (mediators) | queue of the shelters the volunteer administers + DC queue if DC |
| `GET /matches/{id}` | — | details without the family's contact |
| `POST /matches/{id}/decision` | `{decision: accept\|decline, decided_by: person\|operator, note?}` | `200 {family?, whatsapp_link?}` |
| `POST /safe-check` | public `{capture_id, status: safe\|went_to_shelter, reported_shelter_id?, lat?, lon?, family_contact_allowed, language}` | `200` |

### 8.8 Dashboard, alerts, export
| Method and route | Auth | Response |
|---|---|---|
| `GET /dashboard/events` | Val, DC | events with counts |
| `GET /dashboard/events/{id}` | Val, DC | 3.11 |
| `GET /alerts` | admin (alerts of their shelters), DC (all) | list |
| `POST /alerts/{id}/resolve` | recipient admin, DC | `200` |
| `GET /shelters/{id}/export.csv` | admin of a `confirmed` shelter | CSV of the people and deliveries of the own shelter, without thumbnail or `person_id`, audited. **PROPOSAL (to be confirmed)** (Q22) |

---

## 9. Offline synchronization and conflicts

### 9.1 What works offline
Create shelter, check-in by photo (`assisted`), check-in without biometrics, card issuance and reading, editing a person's data, adult exit, delivery by card, rescue, search (PROPOSAL, Q45), address update. **Does not work offline:** login, invites, captures with liveness (includes revalidated high-value delivery and child exit), endorsements, confirmation, mediation, dashboard.

### 9.2 Queue
- Each write becomes an item in `queue` with a UUID `id`, used as the `Idempotency-Key` and, when the entity is new, as the document ID.
- Draining: when the app opens, on the `online` event, right after enqueuing and every 30 s while there are pending items and the app is open (iOS does not have Background Sync).
- FIFO, one item at a time. By result:
  - `2xx` or idempotent response: removes the item and the `Blob`; updates caches.
  - `401`: stops draining and asks for login. Afterwards, it only resends items whose `volunteer_id` is that of the new session; those of another volunteer become `held` with "entre como <nome> para enviar" ("sign in as <name> to send").
  - `409 shelter_nearby` on creation: the item and all those with the same `shelter_id` become `held` until the admin chooses (3.2.1).
  - Image/face `422`: `needs_action`; the app lists it in "Pendências" (Pending issues).
  - `428 liveness_required` from a queued delivery: does not happen (the app sends offline deliveries as `liveness_skipped_offline`, 3.5.2).
  - `429`, `5xx`, `503`, network: `attempts += 1`, `next_attempt_at = now + min(5 s × 2^attempts, 10 min) × (0.5 to 1.5 random)`; respects `Retry-After`.
  - Other `4xx`: `needs_action` with the code.
- "N pendentes" ("N pending") counter always visible; warning if there is an item pending for more than 24 h.

### 9.3 Dependencies
Items that reference a shelter created offline come after it (FIFO). If the shelter is `held`, the dependents are held too.

### 9.4 Conflict resolution (the server is the source of truth [dec. a])
| Case | Rule |
|---|---|
| Same person registered on two phones (offline) | Same token → same `person_id`. The second check-in becomes an update: adds a `stay`; level only goes up. |
| Same person at two shelters at the same time | The `stay` with the most recent `entered_at` becomes `current_shelter_id`; the other closes with `exit_reason = checkin_elsewhere`. Entries less than 1 h apart and shelters > 50 km apart: `anomaly_distance` alert. |
| Same person on two cards issued offline | The first to sync stays; the second receives `409 person_has_card` → `needs_action` ("pessoa já tem o cartão X" ("person already has card X")). |
| Editing a person's data | Last to edit wins, by `client_edited_at`; an older edit is ignored (`200` with the current state). |
| Repeated item (retry) | `Idempotency-Key` returns the original response; no new call to Unico. |
| Two shelters < 100 m apart created offline | 3.2.1. |
| Shelter merge accepted | For each person with a `stay` at the source: changes `shelter_id` source→destination; if there was already an open `stay` at the destination, keeps the one with the oldest `entered_at` and closes the other. Cards, deliveries and rescues: `shelter_id` repointed. Source members become `volunteer` of the destination (`via = merge`). Source: `status = merged`, `merged_into = destination`. Queue items arriving later for the source are applied to the destination. |
| Event merge | 4.3. |
| Write to a shelter of a closed event | `409 event_closed` → `needs_action` (PROPOSAL, Q36). |

---

## 10. Security and privacy

### 10.1 LGPD

#### 10.1.1 Legal basis per operation (stored in `audit_log.legal_basis`)
| Operation | `legal_basis` |
|---|---|
| Check-in of a conscious adult | `consent` (art. 11, I) |
| Check-in of an injured/incapacitated person, rescue, thumbnail | `vital_interest` (art. 11, II, "e") |
| Child check-in and exit | `child_protection` (art. 14, § 3) |
| Donation delivery and revalidation | `fraud_prevention` (art. 11, II, "g") |
| Missing person search and mediation | `vital_interest` |
| Family contact data | `family_consent` (art. 7, I) |
| "Estou Seguro" | `consent` |
| Volunteer accreditation | `fraud_prevention` (**PROPOSAL (to be confirmed)**, Q35) |
| Civil Defense dashboard and operation | `public_policy` (art. 11, II, "b") |

#### 10.1.2 Purge
- Event closed → deletes thumbnails immediately [dec. e].
- `purge_at = closed_at + PURGE_AFTER_DAYS` (PROPOSAL 30, Q9). At purge:
  - people whose events in `event_ids` all have `purge_at ≤ now` (or were merged into one that does): deleted;
  - the event's cards, deliveries, rescues, searches, matches, merges, alerts, memberships, site photos and shelters: deleted;
  - event: keeps only `name`, dates and `stats`, `status = purged`.
- "Estou Seguro" without an event: deleted 30 days after the record (PROPOSAL).
- Volunteer profiles: deleted 365 days after `last_seen_at`, except banned ones (PROPOSAL, Q21).
- Audit: 1-year TTL (PROPOSAL, Q21).
- Photos of people never stay on the server: they only exist in the process memory during the call to Unico [dec. e]. On the phone, the `Blob` is deleted after the `2xx`.

#### 10.1.3 Transparency
Short text on each collection screen (check-in, "Estou Seguro", search): purpose, retention period, who has access, legal basis, controller's name. Consent and "Estou Seguro" in PT, EN and ES [dec. h, M3]. Page `/privacy` with the full text.

#### 10.1.4 Data protection impact assessment (RIPD), region and international transfer
- The repository must contain `docs/pt_br/RIPD.md` before use with real data (art. 38): data processed (ID derived from the token, thumbnail, name, needs, location, card, family contact), purposes, legal bases (10.1.1), risks (abuser, fake shelter, malicious operator, leak), measures (this section), purge.
- Region [dec. h, A8]: the pilot is in the US (`us-east1`). The RIPD records the international transfer (art. 33) and documents that each controller can bring up its own instance in `southamerica-east1` (São Paulo), at the cost of leaving the free tier, with no code change: only the Ansible inventory changes.
- Art. 33 mechanism and definition of controller/processor: Q10.

### 10.2 Protection against fake shelters
- Born `unconfirmed`: sees only what it registered, does not search, does not mediate, does not accept merges, does not export [dec. d].
- The consent screen shows "Abrigo não confirmado pela Defesa Civil" ("Shelter not confirmed by Civil Defense").
- Creation requires a profile with liveness: the creator is identifiable and bannable.
- Remote endorsement does not produce endorsers (2.2). Revocation flags endorsers.
- Shelter < 100 m from another: blocked or `flagged` for Civil Defense.

### 10.3 Protection against an abuser looking for the victim
- Search only by an operator of a confirmed shelter or Civil Defense; limit per volunteer (PROPOSAL, Q8).
- Search response identical with or without a match.
- The mediator is from the shelter where the person is (or Civil Defense), not from the shelter of whoever searched.
- The family's contact only appears to the mediator after the person accepts.
- Location is never sent automatically.
- Card reading (`GET /cards/{code}`) returns only the members' names and level, with no current shelter or history.
- Search writes an audit record with who searched.
- Residual risk: an abuser who is an admin of the same shelter as the victim. Mitigation only through audit; recorded in the RIPD.

### 10.4 Public endpoints and cost
- Public `POST /captures` and `POST /safe-check`: 5 captures per IP per hour (`governor`, in memory) and `PUBLIC_CAPTURES_PER_DAY` in total (in-memory counter, reset at midnight UTC). PROPOSAL (Q15).
- Maximum body of 3 MB; individual image ≤ 800 KB.
- Liveness on delivery only by sampling and for high value reduces consumption of Unico processes [dec. h, A1].

### 10.5 Secrets
`HUMAN_TOKEN_ACCESS_KEY`, `HMAC_KEY` and `THUMBNAIL_KEY` live only in each installation's vault (`inventory/prod/`, outside git) and in `/etc/taba/env` on the VM; `HMAC_KEY` has a backup outside the VM [dec. i, Q3]. Never in the repository (`.env` and `inventory/prod/` are in `.gitignore`).

---

## 11. Interface languages (i18n) [dec. f, h]

### 11.1 General rule
- Interface in `pt-BR` and `en`. Files `pwa/src/i18n/pt-BR.json`, `en.json` and `es.json` (the latter only with the keys from 11.3). New language = new file.
- Detection: first value of `navigator.languages` that starts with `pt` → `pt-BR`; otherwise `en`.
- "PT | EN" selector fixed at the top. Choice saved in `config.language` and, when logged in, in `volunteers.language` (`PATCH /me`). On login, the profile language prevails.
- Links sent (invite, notice to the family) do not carry a language: whoever opens them sees them in their own phone's language.
- WhatsApp message to the family: in the `family.language` given in the search (`pt-BR` or `en`).
- Dates and numbers: `Intl.DateTimeFormat` and `Intl.NumberFormat`.
- Typed data is not translated. Server errors arrive as stable codes and are translated by the app.

### 11.2 What stays in English
Identifiers, routes, logs, commits and PRs (section 0) [dec. j].

### 11.3 Spanish [dec. h, M3]
Only two screens: the consent screen (check-in) and the `/safe` page.
- **PROPOSAL (to be confirmed) (Q43):** these two screens have their own "PT | EN | ES" selector. On the consent screen the selector is independent of the volunteer's language (whoever reads the consent is the person served); the default is the interface language. On `/safe`, the default comes from `navigator.languages` (`es*` → `es`). Outside these screens, whoever chose `es` sees `en`.

---

## 12. Open questions and proposals to be confirmed

Resolved on 2026-10-21 and removed from here: Q1–Q5 [dec. i], Q11 (AGPL-3.0 license), Q12 (repository `github.com/thingsbuilder/taba`) and Q14 (domain `taba.thingsbuilder.com`) [dec. g]. See section 13.

### 12.1 Owner's pending items
- **Q13.** First screen to build. The first task is already defined (journey test on iPhone, [dec. i]); after it, the minimum order is login → create shelter → check-in.

### 12.2 Product and legal questions
- **Q6.** Do Unico's terms allow `api+humantoken` with photos that did not come from the SDK (rescue, search)?
- **Q7.** Unico plan and monthly process limit; who pays.
- **Q8.** PROPOSAL: search only by an operator of a confirmed shelter or Civil Defense; 5 photos per search; 10 searches per volunteer per day; search done by the family outside v1.
- **Q9.** PROPOSAL: purge 30 days after the event is closed.
- **Q10.** Controller and processor; art. 33 mechanism; legal validation of the RIPD.
- **Q15.** PROPOSAL: 5 public captures per IP per hour and 500 per day in total.
- **Q16.** PROPOSAL: delivery duplication per person, not per household (the card groups the household, but the rule counts per person); offline delivery that would require liveness is born in review.
- **Q17.** PROPOSAL: default period of 7 days per item, configurable per shelter.
- **Q18.** PROPOSAL: guardian revalidation failed → exit blocked; admin can record an exit by exception, audited, with an alert to Civil Defense.
- **Q19.** PROPOSAL: no push notification in v1.
- **Q20.** PROPOSAL: revocation does not affect the shelters the revoked one endorsed; it only flags who endorsed it.
- **Q21.** PROPOSAL: audit for 1 year; volunteer profile deleted 1 year after last access, except banned ones.
- **Q22.** PROPOSAL: "view data from other shelters" = history of a person present at your shelter; no free search by name across the network; export only of the own shelter, without `person_id`.
- **Q23.** PROPOSAL: global roles `validator` and `civil_defense` with different powers (2.1), granted via CLI.
- **Q24.** PROPOSAL: person at an unconfirmed shelter or without a shelter → Civil Defense queue; "Estou Seguro" with prior consent for the family to know only that they are safe.
- **Q25.** PROPOSAL: search active until the purge of the event of the originating shelter, compared with each new registration.
- **Q26.** PROPOSAL: 7-day session, renewed on each use.
- **Q27.** Does the selfie at the start of each shift (idea 4 of the history) stay out of v1?
- **Q28.** PROPOSAL: `no_biometrics` record when there is no way to generate a token; stays out of the photo search.
- **Q29.** PROPOSAL: up to 2 site photos per shelter, ≤ 200 KB, in Firestore itself.
- **Q30.** PROPOSAL: "linked to the new shelter" = having or having had a `membership` with it.
- **Q31.** PROPOSAL: a shelter that declared a distant event takes part in the chain; "region or state" = same state (UF) or ≤ 300 km.
- **Q32.** PROPOSAL: no Firestore backup in v1 (scheduled export and PITR have a cost).
- **Q33.** PROPOSAL: thumbnail of a rescued person still without a shelter visible to the landing point volunteer who registered them and to Civil Defense.
- **Q34.** PROPOSAL: photos for token reduced to ≤ 300 KB.
- **Q35.** PROPOSAL: legal basis for volunteer accreditation = art. 11, II, "g".
- **Q36.** PROPOSAL: a shelter of a closed event does not accept new writes.
- **Q37.** PROPOSAL: a removed volunteer only returns to the shelter via in-person QR.
- **Q38.** PROPOSAL: in-person endorsement code expires in 10 min.
- **Q39.** PROPOSAL: anomaly thresholds: 30 check-ins in 10 min; 100 rescues in 10 min (landing point [dec. h, A11]); same person > 50 km apart in < 1 h; rescue outside the event radius.
- **Q40.** PROPOSAL: on an accepted merge, the source's admins become volunteers at the destination.
- **Q41.** PROPOSAL: liveness sampling on 5% of ordinary deliveries, by deterministic draw from the `delivery_id`; the list of high-value items is configured per shelter.
- **Q42.** PROPOSAL: an `assisted` person only receives a donation online after a capture with liveness (which upgrades them to `verified`); offline, the delivery goes to review.
- **Q43.** PROPOSAL: own PT | EN | ES selector on the two screens with Spanish (11.3).
- **Q44.** PROPOSAL: card code with 8 Crockford base32 characters, generated on the phone; a person on at most one active card; the card is valid across the whole network.
- **Q45.** PROPOSAL: search registered offline goes through the queue (the specialist's review recommends not doing it, see 12.3, section 4 item 8).

### 12.3 From the specialist's review — awaiting decision
Findings from the disaster management specialist's review (internal document) that the owner has not decided yet. They are not in the body of the spec.

| ID | Summary | Specialist's recommendation |
|---|---|---|
| C1 | Consent is not free when refusing the selfie reduces shelter or donation; affects `no_biometrics` (Q28). | Rule "refusing the selfie does not take away shelter or donation"; `no_biometrics` as a first-class path, with donation by list and duplication only within the shelter; buttons of equal weight; refusal rate on the dashboard. |
| C2 | Unaccompanied or separated child has no flow; revalidation proves it is the same adult, not the legal guardian. | States with guardian / separated / unaccompanied (names in English if accepted); alert to Civil Defense and to the Guardianship Council (Conselho Tutelar); exit only by a protection role; "declared" or "verified with document" link; a child's match always to the protection queue; confirm with Unico the use with minors. |
| A2 | Duplication per person ignores that food baskets and cleaning kits are per family (disagrees with Q16). | Mark each item as per person or per family; per-family duplication compares the household. |
| A3 | The household is missing as a complete entity (today the card groups members, with no head or family data). | `families` with members, declared head and card number. |
| A4 | Mediation by an untrained volunteer admin; the family's contact stays on personal WhatsApp. | `mediador` (mediator) role granted by Civil Defense/validator; without it, Civil Defense queue; institutional number or instruction to delete the contact; child always in the protection queue. |
| A5 | Search isolated from official channels; search disappears silently at purge (adjusts Q8 and Q25). | Search also by the Red Cross/social assistance; warn whoever registered before deleting a search without a match; say on the screen that it does not replace the police report; deaths out of scope. |
| A6 | Match false positive has no handling. | Mediator checks the family's data with the person before accepting; "not the person" decision (name in English if accepted); measure the rate in the pilot. |
| A7 | Dashboard does not answer what Civil Defense reports (homeless x displaced (desabrigados x desalojados), breakdowns, vacancies). | Capacity and occupancy, count including `no_biometrics` and `assisted`, breakdowns by age group, pregnant women, people with disabilities (PcD), unaccompanied, exits by destination, daily CSV for the bulletin. |
| A9 | Without a controller there is no pilot. | Agreement with a municipality or organization as controller before any real data; Civil Defense designates who receives the global roles. |
| A10 | 30-day purge is short for deliveries with public funds and for unresolved searches (adjusts Q9). | Differentiated purge: biometrics and thumbnail in 30 days; delivery without biometrics for the controller's accountability period; active search forwarded before deletion. |
| A12 | Dependence on one vendor and a free VM at peak. | Documented degraded mode: without Unico or without quota, registration by name/wristband and tokens later; resize the VM before a real event. |
| M1 | Volunteer turnover and shared phones leave items held forever. | Admin takes over another volunteer's held items (audited); session per shelter device with operator switch by local PIN. |
| M2 | Vulnerable groups and identity (dementia, facial injury, trans people, Indigenous people, migrants). | Social name field; structured vulnerability fields; consent read aloud; a sentence stating that the data does not go to the police or immigration. |
| M4 | "Needs" as free text becomes health data. | Closed operational list (continuous medication, dietary restriction, reduced mobility, health follow-up). |
| M5 | Adult exit without a destination. | Exit destination field (closed list) and municipality; on the dashboard. |
| M6 | History from other shelters visible to any volunteer of a confirmed shelter. | History only for admin or social assistance; volunteer sees only "has already been at another shelter: yes". |
| M7 (low) | 100 m radius and poor GPS inside buildings produce many justifications. | Acceptable; record in the pilot how many justifications appear in order to calibrate the radius. |
| Section 3 | Operational gaps: shelter capacity and vacancies; protection role; homeless x displaced; companion animals; contacts of the Guardianship Council and of the local Civil Defense per event; integration with CadÚnico out of v1 (only a cross-referenceable CSV, never the token ID); training material. | Minimal proposals from the table in section 3 of the review. |
| Section 4 | What not to do in v1: the whole "Estou Seguro" (leave it for v2); remote endorsement and confirmation chain; event and shelter merge (Civil Defense resolves by hand in the pilot); site photos and Nominatim; offline search through the queue (Q45). | Cut from the pilot's build order, without changing what the system is. |
| Section 5 | Positions on proposals: adjusts Q8 (include protection role; limit per role), Q9, Q17 (period per item, including "once per event"), Q15 (limit per device + captcha, not per IP), Q16 ("approve all of the day"), Q25; disagrees with Q18 (exception only by protection role or Civil Defense); reservations on Q19 (notice outside the app for children) and Q24 (Civil Defense queue with named people and a deadline); agrees with the others. | See the table in section 5 of the review. |
| Section 6 | Validation plan: bench test with real people, drill with Civil Defense, routine shelter; metrics and criteria for giving up on a feature. | Follow the stages and metrics of section 6 of the review before a real event. |

---

## 13. Decision log

### 13.1 Owner decisions incorporated in this version
| Decision | Where |
|---|---|
| Q1 [dec. i]: offline check-in is born `assisted` and is upgraded on the first capture with liveness | 3.4, 4.1 |
| Q2 [dec. i]: shelter merge is a request accepted by an admin of the destination | 3.2.1, 9.4 |
| Q3 [dec. i]: the database stores the HMAC of the token, never the raw token; `HMAC_KEY` in the vault, with backup | 6.3.2, 10.5 |
| Q4 [dec. i]: liveness through the journey; first task = test the journey in the PWA installed on iPhone; plan B web SDK | 5.1 |
| Q5 [dec. i]: egress above 1 GB/month is a known cost (~US$ 0.12/GB) | 6.1.4, 6.4 |
| Q11: AGPL-3.0 license | `LICENSE` |
| Q12: public repository `github.com/thingsbuilder/taba` | — |
| Q14 [dec. g]: domain `taba.thingsbuilder.com`; project Taba; region `us-east1` | 6, 6.4 |
| C3 [dec. h]: "I need rescue" becomes 193/199 guidance without storing an alert | 3.8 |
| A1 [dec. h]: checking at check-in; delivery by card; liveness only for high value and by sampling | 3.5, 4.7 |
| A11 [dec. h]: rescue registered at the landing point; higher anomaly threshold | 3.6, 4.10 |
| A8 [dec. h]: US in the pilot; region per installation; RIPD documents `southamerica-east1` | 6, 10.1.4 |
| M3 [dec. h]: consent and "Estou Seguro" also in Spanish | 11.3 |
| [dec. j]: implementation, logs, commits and PRs in English | 0 |

### 13.2 Technical decisions
| Decision | Reason |
|---|---|
| Revalidation by comparing derived IDs | Same result and cost as the `+revalidation` flow, without storing the raw token. Rejected `humantoken+revalidation`: it would require the raw token in `references`. |
| Global in-memory lock for shelter creation | One instance; solves the 100 m race. Rejected: transaction with a geographic query (9 queries) and a per-cell lock document (unnecessary complexity). |
| Hourly job inside the process | One less systemd unit. Rejected systemd timer/cron. |
| Generic idempotency via `Idempotency-Key` | The queue only resends HTTP requests; one rule for all routes and no repeated (paid) call to Unico. Rejected a batch `/sync` endpoint. |
| Client-generated IDs for entities created offline | Reference between queue items before sync, and natural deduplication. |
| Deterministic liveness draw from the `delivery_id` | Retrying the same delivery does not change the draw result. |
| Short, typeable card code | Works with an improvised wristband and without a good camera. |
| Site photos in Firestore (bytes) | Avoids Cloud Storage for ~2 small photos per shelter. |
| Search responds the same with or without a match | Even "not found" is information for an abuser. |
| Geohash with 9 cells + haversine | Required by dec. g; simple range queries. |
| Preact + `idb` + `vite-plugin-pwa`; `BarcodeDetector` with `jsQR` as fallback | Small PWA; Safari on iOS does not have `BarcodeDetector`. Rejected React (weight). |
| Mediation via `wa.me` message | No paid SMS/e-mail service. |
| Nominatim called by the phone | Allowed by dec. g; avoids egress on the VM. |
| Infra only through the existing playbooks | `provision.yml`/`configure.yml` already create the VM, Firestore and Caddy; the deployment only adds tasks (service, swap, Caddy with proxy, variables). |

---

## 14. Repository file structure

```
api/                          Rust (cargo), binary `taba`
  Cargo.toml
  src/main.rs config.rs auth.rs unico.rs geo.rs shelters.rs events.rs members.rs
      people.rs cards.rs deliveries.rs rescues.rs searches.rs safe_check.rs
      dashboard.rs jobs.rs audit.rs idempotency.rs
  tests/                      integration with fake Unico and the Firestore emulator
pwa/                          Vite + TypeScript + Preact
  src/main.tsx routes.ts api.ts queue.ts db.ts camera.ts image.ts geo.ts nominatim.ts qr.ts
  src/screens/*.tsx
  src/i18n/pt-BR.json en.json es.json
infra/                        already exists
  README.md
  ansible/playbooks/provision.yml configure.yml   (configure gains: service, swap, deploy)
  ansible/templates/Caddyfile.j2 env.j2           (Caddyfile gains reverse_proxy; env.j2 gains variables)
  ansible/templates/taba.service.j2               (new)
  ansible/inventory/example/                      (in git)
  firestore/firestore.indexes.json                (new)
.env.example                  all variables from 6.2.2, without values
docs/en/spec/spec.md          this file (English translation)
docs/pt_br/spec/spec.md       original (Portuguese; prevails in case of divergence)
docs/pt_br/RIPD.md            before real data
```

Commits and pull requests in English [dec. j].

---

## 15. Acceptance criteria

Automated tests use a fake Unico server (`UNICO_URL`) and the Firestore emulator. No automated test calls the real Unico.

**Unico**
1. `extract_token` returns `None` for missing `value`, `""` and `"  "`; none of these creates a person or volunteer.
2. `429` with "monthly process limit" is not retried and generates an `unico_limit` alert; `429` without that message is retried up to 4 times with increasing wait and then becomes `503 unico_busy` with `Retry-After`.
3. Image > 800 KB → `413 image_too_large` without calling Unico.
4. A capture is consumed only once and only by the `device_id` that created it.
5. (Manual, first task) A journey opened from the PWA installed on iPhone returns via the `callbackUri` and produces a session.

**Shelter and event**
6. `accuracy` 101 m → `422 insufficient_accuracy`.
7. Second shelter 80 m from an `open` one → `409 shelter_nearby` with the creator's first name and no phone if `show_phone = false`; with `other_site_reason` → created `flagged` and `shelter_flagged` alert.
8. A shelter 40 km from a shelter of an active event joins it; at 60 km it creates a new event; within ≤ 50 km of shelters of two events it merges the events and the oldest survives.
9. An event with no writes for 30 days becomes `closed` in the hourly job, with `purge_at`; thumbnails of its people are deleted.
10. Two simultaneous creations 50 m apart: only one is created without conflict.
11. A merge requested by an `unconfirmed` shelter only completes when an admin of the destination accepts; before that the destination does not see the source's data.

**Confirmation**
12. Shelter `confirmed` only by remote endorsement → `403 cannot_endorse`; after an in-person endorsement, it can endorse.
13. In-person endorsement with phones 150 m apart → `422 too_far`.
14. Revocation returns the shelter to `unconfirmed` and flags the endorsers.
15. `unconfirmed` shelter: `POST /searches` → `403`; export → `403`; check-in history without `stays` from other shelters.

**Invites and members**
16. Link with `role = admin` → `422 link_volunteer_only`.
17. QR used twice → second one `410`; QR at 31 min → `410`.
18. Admin removing admin → `403`; last admin leaving → `409 last_admin`.
19. Banned volunteer does not create a session (`403 banned`) nor accept an invite.

**People, cards, deliveries, rescues**
20. Same token in two check-ins → one document in `people`, two `stays`, one open.
21. Offline check-in produces `assisted`; a later capture with liveness of the same person → same document, `verified`.
22. Rescue followed by a check-in with liveness of the same person → same document, `verified`, rescue in the history.
23. Ordinary delivery by card, not sampled, to a `verified` person → `201` with `verification = card`, with no call to Unico.
24. Delivery of a `high_value` item without `capture_id` → `428 liveness_required`; with a capture of another person → `409 revalidation_failed` and nothing stored.
25. The same delivery resent (same `delivery_id`) has the same draw result.
26. Delivery of the same item to the same person within the period at another shelter of another active event → `201` with `status = in_review`, reason `duplicate_in_period`.
27. Offline delivery of a `high_value` item → `in_review`, reason `liveness_skipped_offline`.
28. Person on two active cards → the second receives `409 person_has_card`.
29. `GET /cards/{code}` does not return the current shelter or history.
30. Child exit with a capture of another person → `409 revalidation_failed`, `stay` open.
31. Filling in `name` of a person with a thumbnail deletes `thumbnail_encrypted` in the same write.
32. `GET /people/{id}/thumbnail` by a volunteer of another shelter → `403`; permitted access writes an audit record.
33. 50 rescues by the same volunteer in 10 min do not generate `anomaly_volume`; 50 check-ins do.

**Search, mediation, "Estou Seguro"**
34. `POST /searches` returns an identical body with and without a match.
35. A match of a person at a `confirmed` shelter appears only to that shelter's admins; the family's contact only comes with `decision = accept`.
36. A later check-in of a searched-for person creates a match; repeating it does not create a second match.
37. "Estou Seguro" without prior consent produces a `declined_in_advance` match and no notice.
38. The `/safe` page shows the `tel:193` and `tel:199` buttons and there is no route or collection that stores a rescue request.

**Offline and synchronization**
39. With the network off: create a shelter, 3 check-ins by photo, 1 card, 2 deliveries and 1 rescue; turn it back on: everything syncs in order, the `Blob`s are deleted and the counter returns to zero.
40. Resending the same queue item does not produce a second call to Unico.
41. An item with `401` is not resent with another volunteer's session.

**Languages**
42. Phone in `pt-PT` opens in pt-BR; in `es` it opens in English, except the consent screen and `/safe`, which open in Spanish; a manual choice persists after reloading and, when logged in, on another device.

**Security and implementation**
43. No API response contains the raw Unico token; no Firestore document contains the raw token or a photo of a person (only the encrypted thumbnail).
44. The Unico key, `HMAC_KEY` and `THUMBNAIL_KEY` do not appear in any file of the repository or of the compiled PWA; the repository does not contain a GCP project ID or IP.
45. Routes, collections, fields, enums, environment variables and log messages are in English (code review).
