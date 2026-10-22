# Technical specification — Taba (Human Token for shelters and disaster rescue)

> English translation of `docs/pt_br/spec/spec.md` (spec version 3.1, 2026-10-21). In case of divergence, the Portuguese version prevails. Sources: the owner's decisions (items a–l, recorded in section 13), `docs/pt_br/base legal LGPD.md`, `infra/README.md`, a review by a disaster management specialist and the tests against the real API (these last two are internal documents, outside the repository), and the public documentation of Unico Human Token.
>
> Conventions of this document:
> - **Owner decision (a–l):** requirement. Cited as "[dec. a]" etc. Review findings accepted by the owner are cited with the finding ID, for example "[dec. h, A1]" or "[dec. k, C1]".
> - **PROPOSAL (to be confirmed):** my own solution for something the decisions do not cover. Every open proposal appears in section 12.
> - No marking: technical implementation choice, with the rationale in the decision log (section 13).
> - **Later phase:** design kept in section 16, outside the pilot [dec. k].
> - Technical reading of the LGPD, not a legal opinion.
> - The text of the original is in Portuguese; **every identifier is in English** (section 0).

## Contents

0. Naming conventions and implementation language
1. Vision, goals and out of scope
2. Actors, roles and permission matrix
3. Features and flows
4. Business rules
5. Integration with Unico and degraded mode
6. Architecture and deployment
7. Data model (Firestore)
8. REST API
9. Offline synchronization and conflicts
10. Security and privacy
11. Interface languages (i18n)
12. Open questions
13. Decision log
14. Repository file structure
15. Acceptance criteria
16. Phases after the pilot

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
| global roles: validador (validator), Defesa Civil (Civil Defense), mediador (mediator), proteção (protection) | `validator`, `civil_defense`, `mediator`, `protection` |
| convite (invite) | `invite` / `invites` |
| aval presencial (in-person endorsement) | `endorsement` (`in_person`) / `endorsement_codes` |
| sessão (session) | `session` / `sessions` |
| captura (capture, Unico journey) | `capture` / `captures` |
| pessoa atendida (person served) | `person` / `people` |
| level: verificado, assistido, sem biometria (verified, assisted, no biometrics) | `verified`, `assisted`, `no_biometrics` |
| recusou a selfie (declined the selfie) | `biometrics_declined` |
| nome social (social name) | `social_name` |
| passagem por abrigo (stay at a shelter) | `stay` / `stays` |
| destino da saída (exit destination) | `exit_destination` |
| núcleo familiar (household) | `household` / `households` |
| responsável por criança (child's guardian); vínculo declarado / verificado com documento (link declared / checked against a document) | `guardian` / `guardians`; `declared` / `document_checked` |
| situação da criança (child status): com responsável, separada, desacompanhada (with guardian, separated, unaccompanied) | `child_status`: `with_guardian`, `separated`, `unaccompanied` |
| pulseira (wristband, improvised number) | `wristband` |
| cartão/pulseira com QR do núcleo familiar (household card/wristband with QR) | `card` / `cards` |
| doação (donation, delivery) | `delivery` / `deliveries` |
| item de doação (donation item); por pessoa / por família (per person / per household) | `item`; `per_person` / `per_household` |
| resgate (rescue) | `rescue` / `rescues` |
| busca de desaparecido (missing person search) | `search` / `searches` |
| match; "não é a pessoa" ("not the person") | `match` / `matches`; `not_the_person` |
| alerta (alert) | `alert` / `alerts` |
| contatos de referência do evento (event reference contacts) | `reference_contacts` |
| capacidade do abrigo (shelter capacity) | `capacity` |
| modo degradado (degraded mode) | `degraded` / document `system/status` |
| auditoria (audit) | `audit_log` |
| miniatura (thumbnail) | `thumbnail` |
| não confirmado / confirmado (unconfirmed / confirmed) | `unconfirmed` / `confirmed` |
| aberto / encerrado (open / closed; shelter) | `open` / `closed` |
| ativo / encerrado / descartado (active / closed / purged; event) | `active` / `closed` / `purged` |
| descarte (purge) | `purge` |
| sinalizado (flagged) | `flagged` |
| em revisão (in review) | `in_review` |
| painel (dashboard) | `dashboard` |
| idempotência (idempotency) | `idempotency` |
| "Estou Seguro" ("I'm Safe"), fusão (merge), aval remoto (remote endorsement) (later phase) | `safe_check`, `merge`, `remote` |

---

## 1. Vision, goals and out of scope

### 1.1 Vision
Open source system for shelters and rescue teams in disasters (reference: the Rio Grande do Sul (RS) floods, 2024). It recognizes people by their face, without documents, using Unico's Human Token: the same person always receives the same token. The token does not say who the person is, only that they are the same as before. The system **does not validate identity** and must not be presented as doing so.

v1 is a **pilot**, with no defined controller [dec. k, Q10]. The target is the sheltering phase (from day 3 up to months), in shelters of 50 to 500 people.

### 1.2 Pilot goals
1. A single registry of people served across shelters, with a history that follows the person.
2. **Refusing the selfie does not take away shelter or donation** [dec. k, C1].
3. Household as an entity, with a card/wristband with QR for day-to-day delivery [dec. h, A1; dec. k, A3].
4. Donation per person or per household, depending on the item, with flagging (no blocking) of repeated pickups [dec. k, A2].
5. Protection of unaccompanied or separated children [dec. k, C2].
6. Registration of rescued people at the landing or triage point [dec. h, A11].
7. Volunteer accreditation with liveness.
8. Search for missing people by photo, mediated, never returning the location to the person who searched, connected to official channels [dec. k, A5].
9. Revalidation when a child leaves and on high-value or randomly sampled deliveries [dec. h, A1].
10. Dashboard in the format of what Civil Defense reports [dec. k, A7].
11. Work without internet and without Unico (degraded mode) [dec. k, A12, Q7].
12. Cost within the GCP free tier, except the external IPv4 and the egress traffic of a real event [dec. g, i].

### 1.3 Out of scope for v1
- Native app [dec. a]; integration with S2iD and Cemaden [dec. c].
- Integration with the official registry/CadÚnico, health records and interoperability with the Red Cross (RFL) [dec. k]. v1 only offers CSV (8.8).
- Deaths and body identification (responsibility of the forensic institute (IML) and the forensic police (polícia científica)) [dec. k, A5].
- Counting of displaced people (desalojados, staying with relatives or friends): Taba counts only homeless people (desabrigados), and the dashboard says so [dec. k, A7].
- Selfie at the start of each shift: the login with session is what counts [dec. k, Q27].
- Emergency benefit screening, damage registry, volunteer hours and certificate.
- Push notification [dec. k, Q19]: notices appear on the "Avisos" (Notices) screen; a child alert shows the reference phone number (4.9).
- Search done by the family itself, without an operator [dec. k, Q8].
- Spanish outside the consent screen [dec. h, M3].
- Web backoffice for `validator` and `civil_defense`: granted via command line on the VM [dec. k, Q23].
- Postponed until after the pilot (section 16) [dec. k]: "Estou Seguro"; remote endorsement and confirmation chain; shelter and event merge; site photos.

---

## 2. Actors, roles and permission matrix

### 2.1 Actors

| Actor | How it exists in the system | Authentication |
|---|---|---|
| **Person served** | Document in `people`, member of a `household`. Does not use the app. | None; recognized by face, by card or by name/wristband. |
| **Searching family** | Contact data inside a `search`. Does not use the app. | None. |
| **Volunteer** | `volunteers` + `memberships` with `role = volunteer`. Anyone who has done Human Token with liveness has a profile [dec. b]. Includes the landing point operator [dec. h, A11]. | Session after a journey with liveness; operator switch on the device by local PIN (3.1.1). |
| **Shelter admin** | `memberships` with `role = admin`. The shelter creator is the initial admin [dec. a]. Is the default mediator of the own shelter's matches [dec. k, A4]. | Same as volunteer. |
| **Validator** | `global_roles` contains `validator` (Red Cross, city governments, Civil Defense (Defesa Civil)) [dec. d]. | Same as volunteer. |
| **Civil Defense** | `global_roles` contains `civil_defense`. | Same as volunteer. |
| **Mediator** | `global_roles` contains `mediator` (social assistance, Red Cross), granted by Civil Defense or validator [dec. k, A4]. | Same as volunteer. |
| **Protection** | `global_roles` contains `protection` (Guardianship Council (Conselho Tutelar), social assistance, Red Cross) [dec. k, C2]. | Same as volunteer. |
| **Instance operator** | Whoever administers the VM. Not a role in the app. | SSH on the VM. |

**Global roles [dec. k, Q23, A4]:**
- `civil_defense`: powers of `validator` plus: ban from the network, remove an admin, view the full dashboard, handle the central queue and the protection queue, record a child exit by exception.
- `validator`: confirms and revokes shelters; views the dashboard (read only).
- `mediator`: handles the central queue and the matches of any confirmed shelter; registers searches.
- `protection`: handles the protection queue (child matches, separated or unaccompanied children); records the exit of a separated or unaccompanied child and exits by exception; registers searches.
- `validator` and `civil_defense` are granted and withdrawn by the instance operator with `taba role grant|revoke <volunteer_id> <role>`, with audit [dec. k, Q23].
- `mediator` is granted and withdrawn by Civil Defense or a validator in the app (`POST /v1/volunteers/{id}/roles`) [dec. k, A4]. `protection` is also granted and withdrawn by Civil Defense or a validator in the app [dec. l, Q48].

### 2.2 Shelter trust levels [dec. d]

In the pilot there is only confirmation by validator and by in-person endorsement [dec. k]. Remote endorsement: section 16.2.

| `confirmation` / `confirmation_type` | How it is reached | `can_endorse` |
|---|---|---|
| `unconfirmed` | Creation; revocation | `false` |
| `confirmed` / `validator` | Validator confirms | `true` |
| `confirmed` / `in_person` | Admin of a shelter with `can_endorse`, phones ≤ 100 m apart | `true` |

### 2.3 Permission matrix

Legend: V = shelter volunteer; A = shelter admin; NC = `unconfirmed` shelter; C = `confirmed` shelter; Val = `validator`; DC = `civil_defense`; Med = `mediator`; Prot = `protection`. "Own" = of the shelter in which the actor is an active member.

| Operation | V (NC) | V (C) | A (NC) | A (C) | Val | DC | Med | Prot |
|---|---|---|---|---|---|---|---|---|
| Create shelter (any profile with liveness) | yes | yes | yes | yes | yes | yes | yes | yes |
| Check-in, household, card, delivery, adult exit at own shelter | yes | yes | yes | yes | — | — | — | — |
| View own registrations | yes | yes | yes | yes | — | — | — | — |
| View other shelters' history of a person present at own shelter [dec. k, M6] | no | only "has already been at another shelter: yes/no" | no | yes | — | — | yes, when mediating | yes, when handling |
| Exit of a `with_guardian` child with a `document_checked` link | yes | yes | yes | yes | — | yes | — | yes |
| Exit of a `separated`/`unaccompanied` child, or with a link that is only `declared` [dec. k, C2] | no | no | no | no | — | yes | — | yes |
| Child exit by exception [dec. k, Q18, C2] | no | no | no | no | — | yes | — | yes |
| Register rescue (landing point) | yes | yes | yes | yes | — | yes | — | — |
| Register search by photo [dec. k, A5] | no | yes | no | yes | — | yes | yes | yes |
| Mediate match of a person at own shelter [dec. k, A4] | no | no | no | yes | — | yes | yes | — |
| Handle central queue (NC shelter or no shelter) | no | no | no | no | — | yes | yes | — |
| Handle protection queue (child) [dec. k, C2] | no | no | no | no | — | yes | — | yes |
| View thumbnail of unidentified person at own shelter | yes | yes | yes | yes | — | when handling | when mediating | when handling |
| Approve a high-value or sampled delivery without a selfie [dec. l, Q46] | no | no | yes | yes | — | — | — | — |
| Review flagged delivery; configure items | no | no | yes | yes | — | — | — | — |
| Set shelter capacity | no | no | yes | yes | — | yes | — | — |
| Invite volunteer (QR or link); promote to admin; remove volunteer | no | no | yes | yes | — | remove | — | — |
| Remove admin; ban from the network | no | no | no | no | — | yes | — | — |
| Export own shelter's data | no | no | no | yes | — | — | — | — |
| In-person endorsement | no | no | no | yes, if `can_endorse` | — | — | — | — |
| Confirm or revoke shelter | no | no | no | no | yes | yes | — | — |
| Grant/withdraw `mediator` and `protection` [dec. l, Q48] | no | no | no | no | yes | yes | — | — |
| Event reference contacts | no | no | no | no | yes | yes | — | — |
| Network dashboard and daily CSV | no | no | no | no | read | yes | — | — |
| Rename event | no | no | yes | yes | — | yes | — | — |
| Close own shelter | no | no | yes | yes | — | yes | — | — |

Membership rules [dec. b]: an admin does not remove an admin; the shelter always has at least one active admin; the last admin only leaves after promoting another. An admin may leave if they are not the last one.

---

## 3. Features and flows

"Capture with liveness" = the app requests a capture from the server, opens Unico's journey link (flow `humantoken`), the person takes the selfie, the app returns and the server obtains the token (5.2). Requires internet and Unico to be available [dec. i, Q4].

"Photo from the app camera" = `getUserMedia` + frame captured in a `canvas`, with no gallery access. Produces level `assisted`.

### 3.0 Device prerequisites
1. First access: the app generates `device_id` (UUID v4) and stores it in IndexedDB.
2. On iPhone, the first screen asks to install to the home screen before any registration [dec. a]. The app detects `navigator.standalone !== true` on iOS and blocks shelter creation and offline registrations until installation.
3. On install or sign-in, the app calls `navigator.storage.persist()` [dec. a]. If the answer is `false`, it shows a permanent warning "os dados deste celular podem ser apagados pelo navegador; sincronize com frequência" ("this phone's data may be deleted by the browser; sync often").
4. The app queries `GET /v1/status` when it opens and on each sync; in degraded mode it shows the banner from 5.7.

### 3.1 Volunteer accreditation and login
1. "Entrar" (Sign in) screen. The app calls `POST /v1/captures {purpose: "login"}` and receives `{capture_id, link}`.
2. The app stores `capture_id` in IndexedDB and navigates to `link`.
3. The journey redirects to `https://<domain>/return?capture=<id>`. The app calls `POST /v1/sessions {capture_id}`.
4. Profile exists: receives the session. Does not exist: `422 profile_required`; the app shows the form (name, optional phone, "mostrar meu telefone a quem tentar criar abrigo perto do meu" ("show my phone to whoever tries to create a shelter near mine") [dec. a], language) and resends `POST /v1/sessions {capture_id, profile}`.
5. Banned profile: `403 banned`.
6. Offline or in degraded mode: "Entrar" is disabled with the corresponding message. Whoever already has a session on the device keeps working (3.1.1).
7. Session: 7 days, renewed on each use [dec. k, Q26].

#### 3.1.1 Shared device and operator switch [dec. k, M1]
- The device keeps the sessions of all volunteers who have already signed in on it online (`config.sessions`).
- When signing in online for the first time on the device, the volunteer creates a 6-digit local PIN. The app stores only `PBKDF2-SHA256(PIN, salt, 200,000 iterations)`.
- "Trocar operador" (Switch operator) lists the volunteers with a valid session on the device; switching requires the PIN and works offline. Five wrong PINs in a row delete that volunteer's session on the device.
- The active operator is who signs new writes (`volunteer_id` of the queue item).

### 3.2 Shelter creation [dec. a, b, c]
1. Precondition: valid session (any volunteer).
2. The app reads `navigator.geolocation.getCurrentPosition({enableHighAccuracy: true, timeout: 30000, maximumAge: 0})` and stores `lat`, `lon`, `accuracy`.
3. `accuracy > 100 m`: the app asks the user to go to an open area and try again.
4. Online: the app calls Nominatim (reverse) directly from the phone and fills in the address [dec. k]. Offline: it keeps the coordinates with `address.pending = true` and looks it up later (6.1.6).
5. The operator types the name or nickname and, optionally, the capacity (number of people) [dec. k, A7].
6. The app generates `shelter_id` (UUID v4) and sends `POST /v1/shelters` (directly or through the queue).
7. Server, under the global creation lock (6.2.4):
   1. Validates `accuracy ≤ 100`.
   2. Looks for `open` shelters within ≤ `SHELTER_RADIUS_M`. If it finds one and the request does not carry `other_site_reason`: `409 shelter_nearby` with `{shelter_id, name, distance_m, contact: {first_name, phone?}}`. `phone` only if the creator checked `show_phone`.
   3. With `other_site_reason`: creates with `flagged = true` and a `shelter_flagged` alert for Civil Defense. The dashboard counts these justifications to calibrate the radius [dec. k, M7].
   4. Assigns the event (4.3).
   5. Creates the shelter as `unconfirmed`, the creator's `membership` as `admin`, and writes the audit record.
8. On the `409` response, the app shows "Já existe o abrigo X; procure <primeiro nome>" ("Shelter X already exists; look for <first name>") and the way out "É outro local? Explique." ("Is it a different site? Explain.") (resends with `other_site_reason`).

#### 3.2.1 Offline conflict in the pilot [dec. k]
1. Two shelters created with no signal < 100 m apart: the second one to sync receives the `409`; that shelter's queue items become `held` (9.3).
2. The only way out in the pilot is "Explique" (Explain). The app suggests: "Se for o mesmo local, diga isso; a Defesa Civil resolve." ("If it is the same site, say so; Civil Defense will sort it out.") The shelter is born `flagged`, the items proceed, and Civil Defense resolves it by hand (for example, by closing one of the two).
3. Shelter merge by request (decision Q2): section 16.3.

### 3.3 Volunteer invitation [dec. b]
1. Admin taps "Convidar voluntário" (Invite volunteer), chooses the level (`volunteer` by default, or `admin`) and the form:
   - **In-person QR** (`kind = qr`): single use, expires in 30 min, level `admin` or `volunteer`.
   - **Remote link** (`kind = link`): single use, expires in 24 h, **`volunteer` only**.
2. `POST /v1/shelters/{id}/invites` → `{code, url, expires_at}`. QR and link carry only `https://<domain>/invite/<code>`. Requires internet.
3. The invitee opens the URL; the app shows `GET /v1/invites/{code}` (shelter name, level, whether it is confirmed).
4. The invitee always does a capture with liveness (`purpose: "invite"`). Whoever already has a profile only does this capture, which is the revalidation [dec. b]; whoever does not fills in the profile.
5. `POST /v1/invites/{code}/accept {capture_id, profile?}` → creates the `membership` and returns the session.
6. The admin sees active invites and cancels them (`DELETE /v1/invites/{code}`). Whoever invited sees who joined via the link and can remove them.
7. Rejections: invite expired, used or cancelled (`410 invite_invalid`); banned profile (`403 banned`); already an active member (`409 already_member`); previously removed from that shelter and invited by link (`403 removed_from_shelter`) — only returns via in-person QR [dec. k, Q37].

### 3.4 Shelter check-in
Check-in is the moment the person is checked [dec. h, A1].

**Consent screen** [dec. k, C1, M2; dec. h, M3]:
- Shows purpose, retention period, who has access, who is responsible for the pilot (10.1.3) and, if the shelter is `unconfirmed`, "Abrigo não confirmado pela Defesa Civil" ("Shelter not confirmed by Civil Defense") [dec. d].
- States that **the data is not shared with the police or with immigration** and that **refusing the selfie does not take away shelter or donation**.
- Has a short version to be read aloud and its own PT | EN | ES selector (11.3).
- Two buttons with the same visual weight: "Fazer a selfie" (Take the selfie) and "Prefiro não fazer" (I'd rather not).
- The volunteer marks the legal basis: `consent` (art. 11, I) or `vital_interest` (art. 11, II, "e") if the person is injured or incapacitated.

**Registration data** (`data`), all optional:
- `name`, `social_name` [dec. k, M2], `wristband`, `birth_year`;
- `needs`: closed list `{continuous_medication, dietary_restriction, reduced_mobility, health_follow_up}` (yes/no). The screen instructs not to write a diagnosis [dec. k, M4];
- `vulnerabilities`: `{pregnant, lactating, disability: [mobility|visual|hearing|intellectual|psychosocial|other], indigenous_people?: text, migrant_language?: text}` [dec. k, M2];
- child: see 3.4.1.

**Path A — selfie with liveness (online, Unico available):**
1. Capture with liveness (`purpose: "checkin"`, `shelter_id`).
2. `POST /v1/shelters/{id}/checkins {id, capture_id, data, legal_basis}`.
3. Server, in a transaction on `people/{person_id}`:
   - does not exist: creates with `level = verified`;
   - exists with `level = assisted`: upgrades to `verified` and keeps the history;
   - closes the open `stay` at another shelter (`left_at = now`, `exit_reason = checkin_elsewhere`) and opens the `stay` at the current shelter;
   - looks for active searches with this `person_id` (4.8) and creates matches.
4. Response: `{person_id, level, is_new, history}`. `history` follows the visibility rule of 4.11 [dec. k, M6].

**Path B — photo from the app camera** (injured, unconscious, small child, offline, or liveness failed):
1. The app reduces the photo (6.1.4), stores the encrypted `Blob` in IndexedDB (6.1.7) and enqueues `POST /v1/shelters/{id}/checkins {id, photo_base64, data, legal_basis}`.
2. Until it syncs, the person appears in the local list as "pendente" (pending); household and card can already be issued.
3. On the server, the photo goes to the `api+humantoken` flow → `level = assisted`. The server discards the photo after the call. On receiving `2xx`, the app deletes the `Blob` [dec. a, e].
4. The person is upgraded to `verified` if they do a capture with liveness later; the selfie is **offered, not required** [dec. k, Q42 rejected, C1]. The shelter's "Pendentes de selfie" (Pending selfie) list shows who is `assisted`.
5. Face error (`20532`, inconclusive): the item becomes `needs_action`; the volunteer tries another photo or continues through path C.

**Path C — without biometrics** [dec. k, C1, Q28]:
1. When the person refuses the selfie (`biometrics_declined = true`), when there is no usable face, or in degraded mode, the volunteer registers the person by name and/or wristband: `POST /v1/shelters/{id}/checkins {id, no_biometrics: true, data, legal_basis}`. `person_id` = client UUID, `level = no_biometrics`.
2. It is a first-class path: shelter, household, card and donation are the same as for the others. It does not enter the photo search (there is no token). Donation duplication is checked only by the person's own `person_id`/`household_id` (4.7).
3. If the person accepts the selfie later, or if the photo stayed in the queue in degraded mode: `PUT /v1/people/{person_id}/biometrics {capture_id | photo_base64}` (4.12).
4. The dashboard shows the refusal rate per shelter (`biometrics_declined`) [dec. k, C1].

**Unidentified person** [dec. e]: if the check-in or rescue arrives without `name` and without `wristband`, the app also sends `thumbnail_base64` and the server stores it encrypted (4.6).

**Household and card** (3.5.1): at the end of the check-in, the app offers "Novo núcleo familiar" (New household) or "Juntar a um núcleo existente" (Join an existing household) (by reading the card or choosing from the list).

#### 3.4.1 Child [dec. k, C2]
1. `data.is_child = true` and `data.child_status`: `with_guardian` (arrived with the guardian), `separated` (with an adult who is not the legal guardian) or `unaccompanied` (alone).
2. Guardians: `POST /v1/people/{id}/guardians {guardian_id, link: declared|document_checked}`. The guardian must be `verified`. `document_checked` records who checked the document and when.
3. `separated` or `unaccompanied`: the server immediately creates the `child_separated` alert for the protection queue, for Civil Defense and for the shelter's admins. The alert shows the event's reference phone numbers (Guardianship Council and Civil Defense, `events.reference_contacts`) for the volunteer to call; there is no push [dec. k, Q19].
4. A change of `child_status` (for example, the guardian arrived) is made by `protection` or Civil Defense.
5. Legal basis: `child_protection` (art. 14, § 3).
6. Human Token works for children [dec. l, Q49]. For babies and unidentified children, the thumbnail remains the recognition resource (4.6).

### 3.5 Donations [dec. h, A1; dec. k, A2, A3]

#### 3.5.1 Household and card
- **Household** (`households/{household_id}`, client UUID): members (`member_ids`), declared head (`head_id`), companion animals (`pets: [{species, count}]`, count only) [dec. k, A3]. Created at check-in, online or offline (`POST /v1/shelters/{id}/households`). A person is in at most one household.
- **Card** (`cards/{code}`): belongs to the household (`household_id`). 8-character Crockford base32 code, generated on the phone, readable and typeable, shown as a QR. It works as a printed card, sticker or wristband (it can be improvised with the code handwritten) [dec. k, Q44].
- A household has at most one `active` card. Loss: the volunteer cancels it and issues another (`POST /v1/cards/{code}/cancel {replaced_by}`).
- The card and the household are valid at any shelter in the network.
- Change of members: `PATCH /v1/households/{id} {add_member_ids?, remove_member_ids?, head_id?, pets?}`.

#### 3.5.2 Items
Admin configures (`PUT /v1/shelters/{id}/items`), per item:
- `scope`: `per_person` (clothing, personal hygiene kit, blanket) or `per_household` (food basket, cleaning kit) [dec. k, A2];
- period: `period_days` (default 7) or `once_per_event = true` (mattress, bedding) [dec. k, Q17];
- `high_value` (voucher card, mattress, household appliance) [dec. h, A1].

#### 3.5.3 Day-to-day delivery
1. Volunteer reads the QR with the app camera (or types the code). The app shows the household members (local cache if offline) and the volunteer taps who is picking up.
2. `POST /v1/shelters/{id}/deliveries {id, card_code, person_id, item, quantity, delivered_at}`.
3. Server decides whether to ask for liveness (online and outside degraded mode):
   - `high_value` item;
   - random sample: `sha256(delivery_id)` as a number in [0, 1) less than `DELIVERY_SAMPLE_RATE` (default 0.05) [dec. k, Q41].
   If it asks: `428 liveness_required`. The app does a capture with liveness (`purpose: "revalidation"`, `expected_person_id = person_id`) and resends with `capture_id`. Capture's `person_id` different from the expected one → `409 revalidation_failed`, nothing is stored.
   **Person who does not or cannot take the selfie** (refusal, `no_biometrics`, no usable face) [dec. l, Q46]: the app offers "Aprovação do admin" (Admin approval). An admin of the shelter checks the person against the card and approves on the spot, with their own session (on the device, through the operator switch, 3.1.1). The app resends with `admin_approval: true`; the server requires the sender to be an admin of the shelter (`403 admin_approval_required` if not) and stores `verification = admin_approval`, `approved_by`, `status = ok` and the audit record with the admin's name. It does not go to the review queue.
4. An `assisted` person receives by card like any other; the app offers the selfie, without requiring it [dec. k, Q42 rejected].
5. Duplication [dec. k, A2]: across all active events [dec. c], for `per_person` it looks for a delivery of the same `item` to the same `person_id`; for `per_household`, to the same `household_id`; within the period (or in the same event, if `once_per_event`). If there is one: `status = in_review` with `reasons: [{kind: duplicate_in_period, previous_delivery_id}]`. **The delivery is never blocked due to duplication.**
6. Response: "Entrega registrada" (Delivery recorded) and, if flagged, "Enviada para revisão: outra entrega em <data>" (Sent for review: another delivery on <date>). The response tells nothing of the other delivery (neither the shelter nor the date) to whoever asks from an unconfirmed shelter, except when the other delivery is the shelter's own; to a confirmed shelter it tells the date, and the name of the other shelter only if that one is confirmed too. This also holds for the replay of the same delivery and for the review list.
7. Offline (`offline: true` is the app's claim: the server accepts it only when `delivered_at` is at least 30 s older than the receipt, and records it in `offline_claimed` and in the audit) or in degraded mode: the card works from the cache and the random sample does not run. A delivery of a `high_value` item uses the admin approval, if there is an admin session on the device; without an admin, it is stored with `verification = card` and `status = in_review`, reason `liveness_unavailable`.
8. Admin reviews at `GET /v1/shelters/{id}/deliveries?status=in_review` and decides `reviewed_ok` or `irregular`, with a note.

### 3.6 Registration of rescued people [dec. h, A11]
Registration is done **at the landing or triage point**, not in the boat.
1. Logged-in volunteer taps "Resgate" (Rescue). Photo only from the app camera.
2. The app records: date and time, GPS of the point (`lat`, `lon`, `accuracy`), `volunteer_id`, `device_id`.
3. Optional: name, wristband, note, point name (`landing_point`), destination shelter (`destination_shelter_id`).
4. The screen shows nearby open shelters with vacancies (`GET /v1/shelters/nearby`), for referral [dec. k, A7].
5. No name and no wristband: thumbnail (4.6).
6. Goes through the queue: `POST /v1/rescues`. On the server, `api+humantoken` → `person_id`; creates the person as `assisted` if they do not exist, or adds the rescue to the history. No usable face or in degraded mode: the person is created as `no_biometrics` and the photo stays in the queue as `PUT /v1/people/{id}/biometrics` (4.12). Event: that of the nearest `open` shelter within ≤ `EVENT_RADIUS_KM`; otherwise null.
7. When the person checks in at any shelter, the token is the same and the rescue appears in the history.
8. Matches with active searches are also looked for here.
9. Anomalies (4.10), with a threshold specific to rescue.

### 3.7 Missing person search [dec. k, A5, Q8, Q45]
1. Who registers: volunteer of a `confirmed` shelter, Civil Defense, `mediator` or `protection` [dec. d; dec. k, A5].
2. The screen states, before anything else: **"Esta busca não substitui o registro de desaparecimento na delegacia."** ("This search does not replace the missing person report at the police station.") [dec. k, A5].
3. The operator records: name, nickname and notes of the missing person; name, phone, relationship and language of the family; 1 to 5 photos (gallery allowed, this is the exception). Limit: `SEARCHES_PER_DAY` searches per volunteer per day (default 10), counted on the day of the `TZ_PILOT` time zone (default `America/Sao_Paulo`): the quota starts again at midnight there, not at 21:00 [dec. k, Q8].
4. `POST /v1/searches`. With no signal, it goes through the queue; the photos stay **encrypted on the device** (6.1.7) and are **deleted right after sending** (`2xx`) [dec. k, Q45].
5. Server generates one token per photo (`api+humantoken`), stores only the derived `person_id`s (`searches.person_ids`, "search" level: does not become a person) and compares with `people`. A match on any photo counts. In degraded mode, the search waits in the queue.
6. **The response is always the same**, with or without a match: `202 {message: "search_registered", usable_photos}`. The app shows "Busca registrada. Se a pessoa for encontrada e concordar, a equipe entrará em contato com a família." ("Search registered. If the person is found and agrees, the team will contact the family.") Not even the operator learns whether there was a match.
7. `usable_photos` says how many photos produced a token, so the operator can ask for others.
8. The search stays active until the purge of the event of the originating shelter; each later check-in or rescue is compared with the active searches [dec. k, Q25].
9. **Before deleting** [dec. k, A5, Q25]: when the event is closed, each active search without an accepted match generates the `search_expiring` alert for whoever registered it, with the purge date and the instruction to forward it to the police station and to the Red Cross.
10. Match → mediation (4.8).
11. **Family link** [owner's decision, phase 4; optional]: instead of typing the search, the operator makes a one-use link valid for 12 hours (`POST /v1/search-links`). The family opens `/f/<code>` (public, no login, PT | EN | ES) and sends the data and 1 to 5 photos from their own gallery (`POST /v1/search-links/{code}/submit`), which goes through the same pipeline as item 5 and has the same response as item 6. The search is in the name of the operator who made the link (`via = family_link`), with their daily limit, audit and mediation; the operator does not read what was sent. No offline queue; in degraded mode the submit answers `503 unico_degraded` and the link stays valid. The link is only consumed when at least one photo produced a token: if none served, the submit answers the same `202` with `usable_photos: 0`, nothing is registered (no search, no notice), the try counts toward the cap of 5 per link and the link stays valid; the page says "No photo worked; send other photos". The origin shelter of the search is the `shelter_id` of the link: whoever has a `confirmed` shelter uses it by default; the Civil Defense, `mediator` and `protection` choose any `confirmed` shelter of an active event (`422 invalid_shelter` if it does not exist or is not confirmed; `409 event_closed` if the event is over).

### 3.8 Revalidation
Revalidation = a new capture with liveness whose resulting `person_id` is compared with the expected one (5.1).

**Donation delivery:** only `high_value` items and random sample (3.5.3) [dec. h, A1]; without a selfie, admin approval [dec. l, Q46].

**Child exit** [dec. k, C2, Q18]:
1. Volunteer opens the child and taps "Saída" (Exit). Chooses the guardian who is picking up (from `guardians`).
2. Who may record it: see matrix 2.3. Volunteer and admin only record the exit of a `with_guardian` child whose link with that guardian is `document_checked`; in the other cases, only `protection` or Civil Defense (`403 protection_role_required`).
3. Capture with liveness of the adult (`purpose: "revalidation"`, `expected_person_id = guardian_id`).
4. `POST /v1/shelters/{id}/exits {person_id, guardian_id, capture_id, exit_destination, destination_municipality?}`.
5. Capture's `person_id` = `guardian_id`: exit recorded, `stay` closed.
6. Different: `409 revalidation_failed`; the exit is not recorded and an alert goes to the protection queue. Only `protection` or Civil Defense record an exit by exception (`exception: {reason}`), audited, with an `exceptional_exit` alert to Civil Defense [dec. k, Q18, C2].

**Adult exit** [dec. k, M5]: no revalidation, `POST /v1/shelters/{id}/exits {person_id, exit_destination, destination_municipality?}`. `exit_destination`: `own_home`, `relatives_or_friends`, `rental_aid`, `other_shelter`, `other`, `unknown`.

### 3.9 Shelter confirmation [dec. d; dec. k]
**By validator:** `POST /v1/shelters/{id}/confirm`. `confirmation_type = validator`, `can_endorse = true`.

**In-person endorsement:**
1. Admin of the new shelter generates a code: `POST /v1/shelters/{id}/endorsements/in-person/code {lat, lon, accuracy}` → `{code, expires_at}` (10 min [dec. k, Q38]), shown as a QR.
2. Endorsing admin (of a `confirmed` shelter with `can_endorse`, who is not an active member of the new shelter) reads the QR with the phone camera, which opens `/endorse/<code>`: `POST /v1/endorsements/in-person {code, endorser_shelter_id, lat, lon, accuracy}`.
3. Server requires both `accuracy ≤ 100` and a distance between the two phones ≤ `SHELTER_RADIUS_M`. Confirms with `confirmation_type = in_person`, `can_endorse = true` and records who endorsed.

**Revocation:** validator or Civil Defense, `POST /v1/shelters/{id}/revoke {reason}` → `unconfirmed`, `can_endorse = false`; whoever endorsed becomes `flagged` and generates an `endorser_flagged` alert. Effect on shelters that the revoked one endorsed: section 16.2 (Q20, postponed).

Remote endorsement: section 16.2.

### 3.10 Civil Defense dashboard [dec. k, A7]
`GET /v1/dashboard/events` and `GET /v1/dashboard/events/{id}`. The dashboard states at the top: **"Conta desabrigados (pessoas em abrigo). Desalojados (em casa de parentes ou amigos) não estão aqui."** ("Counts homeless people (people in shelters). Displaced people (staying with relatives or friends) are not here.")

Per event and per shelter:
- shelters: name, confirmation, `flagged`, position, **capacity, occupancy (open `stays`) and vacancies**;
- unique people, **including `assisted` and `no_biometrics`**, with the proportion of each level;
- breakdowns: age group (0–11, 12–17, 18–59, 60+, by `birth_year`), pregnant women, lactating women, people with disabilities by type, `separated` and `unaccompanied` children, Indigenous people, migrants;
- households and companion animals;
- movements between shelters (`stays` closed by `checkin_elsewhere` in the last 24 h and 7 days) and **exits by `exit_destination`** [dec. k, M5];
- rescues; open alerts; matches in the central and protection queues;
- pilot indicators: selfie refusal rate [dec. k, C1]; `not_the_person` matches [dec. k, A6]; "other site" justifications [dec. k, M7]; Unico processes per person served;
- shelters awaiting endorsement.

Daily CSV for the bulletin: `GET /v1/dashboard/events/{id}/daily.csv` (one row per shelter, with the counts above; no name or `person_id`). Counts via Firestore `count()` aggregation.

The event's reference contacts (`reference_contacts: {civil_defense_phone, child_protection_phone}`) are set by Civil Defense or a validator (`PATCH /v1/events/{id}`) [dec. k].

---

## 4. Business rules

### 4.1 Trust levels of people

| `level` | Origin | Unlocks | Transitions |
|---|---|---|---|
| `verified` | Capture with liveness | Shelter, donation, being a child's guardian | final |
| `assisted` | Photo from the app camera | Shelter, donation by card [dec. k, Q42 rejected], location and reunification | → `verified` on the first capture with liveness, with no new registration [dec. i, Q1] |
| `no_biometrics` | Record by name/wristband (refusal, no face, degraded mode) [dec. k, C1] | Shelter and donation; outside the photo search | → `assisted` or `verified` via `PUT /people/{id}/biometrics` (4.12) |
| "search" | Family photo | Nothing; stays only in `searches.person_ids` | does not become a person |

Level never goes down. Decision k updates the original levels table: `assisted` and `no_biometrics` receive donations.

### 4.2 Shelter
- `status`: `open` → `closed` (admin or Civil Defense).
- `confirmation`: 2.2 and 3.9.
- `unconfirmed` sees only what it registered itself; people registered in it enter the network search normally [dec. d].
- Minimum radius between open shelters: `SHELTER_RADIUS_M` (default 100) [dec. a].
- Required accuracy: `accuracy ≤ 100 m` on creation and on in-person endorsement.
- `capacity` optional (people); vacancies = `capacity − open stays`.

### 4.3 Automatic events [dec. c; dec. k]
On creation of a shelter online (or on sync of one created offline):
1. Looks for `open` shelters of `active` events within ≤ `EVENT_RADIUS_KM` (default 50):
   - none: creates an event with the automatic name "Cidade/UF – mês/ano" (City/State – month/year); with no address yet, "Evento – mês/ano" (Event – month/year), redone when the address arrives if `auto_named` is still `true`;
   - one event: joins it;
   - two or more (**pilot: detection and warning** [dec. k]): joins the event of the nearest shelter and generates the `event_overlap` alert for Civil Defense, which resolves it by hand. Automatic event merge is in section 16.3.
2. An admin of any shelter of the event, or Civil Defense, renames it (`PATCH /v1/events/{id}`) → `auto_named = false`.
3. Activity: any write to a shelter of the event updates `events.last_activity_at`, at most once per hour (in-memory cache).
4. Automatic closing (hourly job, 6.2.6): an `active` event with all shelters `closed`, or with `now − last_activity_at ≥ INACTIVITY_DAYS` (default 30) → `closed`, `closed_at = now`, `purge_at = now + PURGE_AFTER_DAYS`.
5. A `closed` event does not reopen; a shelter of a closed event does not accept writes (`409 event_closed`) [dec. k, Q36].
6. The event organizes but does not isolate: search and donation duplication look at all active events.
7. The question "Este abrigo recebe pessoas de algum destes eventos?" ("Does this shelter receive people from any of these events?") (distant shelter) moves to section 16.3 together with Q31.

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
- Who sees it (`GET /v1/people/{id}/thumbnail`): active members of the `current_shelter_id`, whoever handles an open match about the person (admin, `mediator`, `protection` or Civil Defense) and — [dec. l, Q33] — if the person does not have a shelter yet, the landing point volunteer who recorded the rescue and Civil Defense. Each access writes an audit record.
- Deleted in the same write that fills in `name` or `wristband`, or when all of the person's events are closed. Whichever comes first.

### 4.7 Donations
- Duplication [dec. k, A2]: `(person_id, item)` for `per_person`; `(household_id, item)` for `per_household`. Window: `period_days` or the event (`once_per_event`). Searches across all active events.
- `no_biometrics`: the same rule; since the `person_id` is local, in practice the per-person check is restricted to what was recorded for that person [dec. k, C1].
- Duplication → `in_review`, never blocking.
- Liveness on delivery: `high_value` item and random sample [dec. h, A1; dec. k, Q41]. No selfie possible: admin approval, recorded in the audit [dec. l, Q46].
- `verification`: `card` (default), `liveness` (revalidated) or `admin_approval` (approved by the admin, with `approved_by`). Review reasons: `duplicate_in_period` and `liveness_unavailable` (offline or degraded mode with no admin on the device).

### 4.8 Search mediation [dec. k, A4, A6, C2, Q24]
**Never return location to whoever searched.** When creating a match (`matches/{sha256(search_id|person_id)}`, idempotent), the server chooses the queue:

| Situation of the person found | `mediator_kind` | Who handles it |
|---|---|---|
| Child (`is_child = true`) in any situation | `protection` | `protection` and Civil Defense [dec. k, C2] |
| `current_shelter_id` `confirmed` | `shelter` | Admins of that shelter (default mediator) and any `mediator` [dec. k, A4] |
| `current_shelter_id` `unconfirmed`, or no shelter (rescue only) | `central` | `mediator` and Civil Defense [dec. d; dec. k, Q24] |

Whoever decides first (admin or `mediator`) closes the match for the others; the `mediator` role applies to the whole network [dec. l, Q47].

Flow:
1. Sees the notice in "Avisos" (Notices) (`GET /v1/matches`): name, nickname, relationship and notes given by the family; name of the person in the registry; thumbnail if any; shelter. **Does not see the family's contact yet.**
2. **Checks with the person** the data given by the family before any decision [dec. k, A6]. The screen shows this data as a checklist.
3. The person decides; if incapacitated, whoever handles it decides (`decided_by = operator`).
4. `POST /v1/matches/{id}/decision {decision: accept|decline|not_the_person, decided_by: person|operator, note}`.
5. `accept`: the response carries `family {name, phone, relationship, language}` and `whatsapp_link` (`https://wa.me/<phone>?text=<mensagem>`, in the family's language). The screen instructs: **"Depois de combinar o contato, apague este número do seu celular."** ("After arranging the contact, delete this number from your phone.") [dec. k, A4]. Location is never sent automatically.
6. `decline`: nothing is sent; the search remains active for other people, with no new match for this `person_id`.
7. `not_the_person` [dec. k, A6]: false positive. Closes the match without counting as a refusal; the search remains active and generates no new match for this `person_id`. Counts in the dashboard indicator.
8. Every decision writes an audit record with `legal_basis = vital_interest`.

### 4.9 Child [dec. k, C2, Q19]
- `child_status`: `with_guardian`, `separated`, `unaccompanied` (3.4.1).
- `guardians: [{person_id, link: declared|document_checked, checked_by?, checked_at?}]`; the guardian must be `verified`.
- Revalidation at exit only counts as a guardian check when the link is `document_checked` (3.8).
- `separated`/`unaccompanied` → immediate `child_separated` alert, with the event's reference phone numbers.
- Child match → `protection` queue.
- `legal_basis = child_protection` (art. 14, § 3).

### 4.10 Audit and anomalies [dec. k, Q39]
Every write and every access to a thumbnail or to match data writes `audit_log`. In the pilot, anomalies are **informational only**: they generate an alert with `informational = true`, which appears on the dashboard and not in the admins' "Avisos" (Notices) box. Thresholds:
- `anomaly_volume`: check-ins by the same volunteer above `ANOMALY_CHECKINS_PER_10MIN` (default 30); rescues above `ANOMALY_RESCUES_PER_10MIN` (default 100) [dec. h, A11].
- `anomaly_distance`: the same `person_id` recorded at two points more than 50 km apart less than 1 h apart.
- `out_of_area`: rescue more than `EVENT_RADIUS_KM` from all shelters where the volunteer is a member.

### 4.11 History visibility [dec. d; dec. k, M6, Q22]
- `unconfirmed` shelter: only the own shelter's `stays`.
- `confirmed` shelter: admin sees `stays` at other shelters (shelter and dates); volunteer sees only `has_other_stays: true|false`.
- `mediator`, `protection` and Civil Defense see the person's history only while handling a match or alert about them.
- There is no free search by name across the network [dec. k, Q22].

### 4.12 Late biometrics (`PUT /people/{id}/biometrics`) [dec. k, A12, C1]
Used when a `no_biometrics` person accepts the selfie later or when the photo stayed in the queue in degraded mode.
1. Input: `capture_id` (becomes `verified`) or `photo_base64` (becomes `assisted`).
2. The server derives `token_person_id` (HMAC).
3. If `people/{token_person_id}` does not exist: creates it with the data of the old record, repoints references (`households.member_ids`, `head_id`, `deliveries.person_id`, `rescues.person_id`, `guardians`), deletes the old record and stores `former_ids: [<uuid>]`.
4. If it already exists (the person was registered elsewhere): merges the `stays` and repoints the references the same way. If the two are in different households, keeps the household of the record with a token and generates a `household_conflict` alert for the admins of the current shelter.
5. Looks for active searches and creates matches.
6. All in one transaction per person; response `{person_id, level}`. The app replaces the ID in the local cache.

---

## 5. Integration with Unico and degraded mode

### 5.1 Flows used

| Screen | Flow | Level |
|---|---|---|
| Login, invite, online check-in, revalidation (delivery and child exit), late biometrics with selfie | `humantoken` (Journey, with liveness) | `verified` |
| Check-in by photo, rescue, late biometrics by photo | `api+humantoken` (without liveness) | `assisted` |
| Family search | `api+humantoken` | search |
| — | `api+liveness+humantoken*`: **not used**; rejects ordinary JPEG with `400 INVALID_CAPTURE_TOKEN` (test of 2026-10-20) | — |
| — | `humantoken+revalidation`, `api+liveness+humantoken+revalidation`: **not used** (13) | — |

Unico's terms allow sending a photo that did not come from the SDK [dec. k, Q6].

**Revalidation without the revalidation flow.** A new `humantoken` journey and comparison of the derived `person_id`. Same result and cost as the `+revalidation` flow, without storing the raw token (6.3.2).

**Journey instead of SDK** [dec. i, Q4]. **First implementation task** [dec. k, Q13]: test the journey from the PWA installed on iPhone (navigation outside the app scope in standalone mode, camera permission, return via `callbackUri`); then, login → create shelter → check-in. Plan B: Unico's web SDK.

**Test result (2026-10-21, iPhone, installed PWA): validated; plan B is not needed.**
- The journey opens in a Safari window over the app (iOS behavior for out-of-scope URLs). Same window and new window give the same result: the PWA uses the same window.
- The camera works inside that window, and the liveness capture ends with `PROCESS_STATE_FINISHED` and a token.
- A `callbackUri` inside the PWA scope closes the Safari window and returns to **the app itself**, as a fresh page load.
- So the `callbackUri` carries its own identifier (nonce) and the return screen looks the process up by it, without relying on in-memory state or `localStorage`.
- The journey shows the organization name registered in the Unico console ("<organization> needs the following information"). The installation should use the name "Taba" and, if possible, the logo, to avoid distrust.

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
- `429` with message `monthly process limit ... reached`: the credits have run out. Do not retry; enters degraded mode with reason `limit` (5.7) [dec. k, Q7]; `unico_limit` alert for Civil Defense; `error` log.
- `429` without that message: exponential backoff with jitter on the server (1, 2, 4, 8 s + 0–1 s random), at most 4 attempts. After that, `503 unico_busy` with `Retry-After: 30`; the app queue reschedules (9.2).
- `500`/timeout (60 s): one new attempt after 2 s; then `502 unico_unavailable`. Three failures in a row → degraded mode with reason `unavailable` for 5 min.
- `401`/`403 PERMISSION_DENIED`: `500 unico_misconfigured` and `error` log.
- Concurrency: semaphore of 4 simultaneous calls to Unico in the process.
- There is no sandbox: every call is production. Automated tests use a fake server (`UNICO_URL`); real calls only in manual testing.

### 5.6 Key
`HUMAN_TOKEN_ACCESS_KEY` (format `uk_live_...`) only in the VM environment file (6.4) or in the local `.env`. It never goes to the phone [dec. a, g]. Rotation through the Unico API invalidates the old one immediately: update the vault, run `configure.yml` and restart the service.

### 5.7 Degraded mode [dec. k, A12, Q7]
"Without Unico" does not mean "without the system".
- State in `system/status {unico: ok|degraded, reason?: limit|unavailable, since, until?}`, read via `GET /v1/status` (public, no personal data).
- `limit`: lasts until the instance operator runs `taba unico resume` (after reloading credits). The server makes no test call, because every call is paid.
- `unavailable`: comes back on its own after 5 min; the next real call tests it.
- In degraded mode the server does not call Unico and responds `503 unico_degraded` to the routes that depend on it.
- In the app, a fixed banner: "Reconhecimento facial indisponível. O cadastro continua por nome e pulseira." ("Facial recognition unavailable. Registration continues by name and wristband.")
  - Check-in and rescue continue through the path without biometrics (3.4, path C); if there is a photo, it stays encrypted in the queue as `PUT /people/{id}/biometrics` until Unico comes back.
  - Delivery by card continues normally; a `high_value` item uses the admin approval or, with no admin on the device, goes to review (`liveness_unavailable`).
  - Search waits in the queue.
  - Login and invite become unavailable; whoever has a session on the device continues (3.1.1).

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
- **Design system** [dec. l]: the "Acolhida" alternative — Outfit for headings, Figtree for body text, IBM Plex Mono for card and wristband codes, rounded corners, soft colors, light and dark themes. The tokens live in `design/tokens.json` and `design/tokens.css`, at the repository root; the PWA imports `tokens.css`. This spec does not detail the design system.
- **QR:** generation with `qrcode`; reading with `BarcodeDetector` when available and `jsQR` as a fallback (Safari on iOS does not have `BarcodeDetector`).
- **Routes:** `/`, `/login`, `/return`, `/switch-operator`, `/shelters/new`, `/shelter/:id`, `/shelter/:id/checkin`, `/shelter/:id/people`, `/shelter/:id/households/:household_id`, `/shelter/:id/delivery`, `/shelter/:id/exit`, `/shelter/:id/members`, `/person/:id`, `/rescue`, `/search`, `/inbox`, `/invite/:code`, `/dashboard`, `/endorse/:code`, `/privacy`.

#### 6.1.1 IndexedDB (`taba`, version 1)
| Store | Key | Content |
|---|---|---|
| `config` | name | `device_id`, `language`, `active_volunteer_id`, `sessions` (per volunteer: secret, `expires_at`, `pin_hash`, `pin_salt`, `failed_pins`), `photo_key` (non-extractable `CryptoKey`) |
| `queue` | `id` (UUID) | `{method, path, body, volunteer_id, shelter_id?, created_at, attempts, next_attempt_at, state: pending\|held\|needs_action, error?, taken_over_from?}` |
| `blobs` | `queue_id` | Pending photos, **encrypted** (6.1.7), deleted when the item receives `2xx` |
| `shelters` | `id` | The volunteer's shelters (cache) |
| `people` | `id` | People of the current shelter (offline cache by name/wristband), including local pending ones |
| `households` | `id` | Households of the current shelter and their members (offline cache) |
| `cards` | `code` | Cards of the current shelter (offline cache) |
| `captures` | `capture_id` | Capture in progress (to resume after returning from the journey) |
| `addresses` | `shelter_id` | Coordinates awaiting Nominatim |

#### 6.1.2 Persistence
`navigator.storage.persist()` on the first logged-in use; result shown in "Configurações" (Settings).

#### 6.1.3 Camera
`getUserMedia({video: {facingMode: "user" | "environment"}})`, frame in `canvas`. No `<input type="file">` in check-in and rescue. In search, `<input type="file" accept="image/*" multiple>`.

#### 6.1.4 Image reduction
`canvas` → `toBlob("image/jpeg", q)`:
- Photo for token: longest side 1280 px; quality starts at 0.9 and drops by 0.05 until the file is **below 800 KB** (the API limit), so as not to harm quality at Unico [dec. k, Q34]. Egress traffic above 1 GB/month is a known cost of a real event (~US$ 0.12/GB) [dec. i, Q5].
- Thumbnail: 128×128, quality 0.6.

#### 6.1.5 Geolocation
`getCurrentPosition` with `enableHighAccuracy: true`; always stores `accuracy`.

#### 6.1.6 Nominatim [dec. k]
`GET https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=..&lon=..&accept-language=pt-BR`, at most 1 request per second (Nominatim policy), result stored in IndexedDB. No signal: stores in `addresses` and tries when it comes back; once obtained, `PATCH /v1/shelters/{id} {address}` through the queue.

#### 6.1.7 Encrypted photos on the device [dec. k, Q45]
- Every photo waiting in the queue (search, check-in, rescue, late biometrics) is stored encrypted with AES-GCM (WebCrypto), with a `photo_key` generated on first use as a **non-extractable** `CryptoKey` and kept in IndexedDB.
- The encrypted `Blob` is deleted right after the item's `2xx`.
- The protection applies against reading the browser's files outside the app; not against someone using the unlocked app itself.

### 6.2 Rust API
A single `taba` binary with the subcommands `serve` (API + hourly job), `role` (`validator` and `civil_defense` roles) and `unico resume` (leaves degraded mode caused by the limit).

**Crates:** `axum`, `tokio`, `tower-http` (6 MB body limit, trace), `serde`/`serde_json`, `reqwest` (rustls), `firestore` (gRPC; authentication through the VM's service account via the metadata server; emulator via `FIRESTORE_EMULATOR_HOST` in tests), `geohash`, `hmac` + `sha2`, `aes-gcm`, `rand`, `base64`, `uuid`, `time`, `tracing` + `tracing-subscriber`, `anyhow`.

#### 6.2.1 Modules
`main.rs` (CLI and router), `config.rs`, `auth.rs` (sessions, volunteer extractor, role check), `unico.rs` (client and degraded mode), `geo.rs` (geohash and haversine), `shelters.rs`, `events.rs`, `members.rs` (invites, memberships, global roles), `people.rs` (check-in, exits, thumbnail, late biometrics), `households.rs` (households and cards), `deliveries.rs`, `rescues.rs`, `searches.rs` (searches and matches), `dashboard.rs`, `jobs.rs` (closing and purge), `audit.rs` (audit and anomalies), `idempotency.rs`.

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
| `PILOT_RESPONSIBLE` | — | Name and contact of the person responsible for the pilot, shown on the collection screens (10.1.3) |
| `SHELTER_RADIUS_M` | 100 | [dec. a] |
| `EVENT_RADIUS_KM` | 50 | [dec. c] |
| `INACTIVITY_DAYS` | 30 | [dec. c] |
| `PURGE_AFTER_DAYS` | 30 | Biometrics, people and searches [dec. k, Q9, A10] |
| `DELIVERY_RETENTION_DAYS` | 90 | Pseudonymized deliveries [dec. k, A10; dec. l, Q50] |
| `SESSION_DAYS` | 7 | [dec. k, Q26] |
| `SEARCHES_PER_DAY` | 10 | Per volunteer [dec. k, Q8] |
| `TZ_PILOT` | `America/Sao_Paulo` | Time zone (IANA name) whose day the daily search quota follows |
| `DELIVERY_SAMPLE_RATE` | 0.05 | [dec. k, Q41] |
| `ANOMALY_CHECKINS_PER_10MIN` | 30 | [dec. k, Q39] |
| `ANOMALY_RESCUES_PER_10MIN` | 100 | [dec. k, Q39] |
| `PURGE_ENABLED` | `false` | The discard (10.1.2) deletes for real only with `true`; off, the hourly task only counts what it would delete (dry run) |

#### 6.2.3 Error format
`{"error": "<stable_code>", "detail": {...}}`. The app translates the code; the server does not translate text. Log messages in English [dec. j].

#### 6.2.4 Concurrency and uniqueness
- **Person and volunteer uniqueness:** by the document ID (6.3.2) in a `get` → `create`/`update` transaction [dec. g].
- **Shelter creation:** a global `tokio::sync::Mutex` in the process. `// ponytail: global lock; fine with the single e2-micro instance. With more than one instance, switch to a transaction on a per-geohash-cell lock document.`
- **Matches:** deterministic ID (`sha256(search_id|person_id)`) and a `create` that fails if it already exists. The decision is stored in a transaction that requires `status = open` (whoever decides first closes it).
- **Households and cards:** `cards/{code}` created with `create` (fails if the code already exists; the app generates another). "One person in one household" and "one active card per household" are checked in the same transaction.
- **Late biometrics:** transaction per person (4.12).

#### 6.2.5 Authentication
- Session: secret of 32 random bytes (base64url) returned to the app; in Firestore, `sessions/{sha256(secret)}`. Header `Authorization: Bearer <secret>`.
- Expires in `SESSION_DAYS`, renewed on each authenticated request when more than 1 day has passed since the last renewal.
- A ban deletes the volunteer's sessions.
- The `Volunteer` extractor loads the profile, `global_roles` and active `memberships`; each handler checks role and shelter state according to the matrix in 2.3.
- The operator switch PIN is local only (3.1.1); the server does not know it.

#### 6.2.6 Hourly job (closing and purge)
`tokio` task inside `serve`, every hour:
1. Closes events (4.3.4) and generates `search_expiring` for the active searches without an accepted match of the closed event (3.7).
2. Deletes thumbnails of people whose events are all closed.
3. Purges (10.1.2) in batches of 400 writes, resuming in the next hour if it exceeds 15,000 deletions in the day (free tier: 20,000/day).
4. Expires overdue captures, invites and endorsement codes (those that do not use TTL).
5. Leaves an expired `unavailable` degraded mode.

### 6.3 Firestore
The database is `(default)`, in native mode, in the installation's region — the only one that uses the free quota (`provision.yml`). No backup in the pilot (scheduled export and PITR have a cost) [dec. k, Q32].

#### 6.3.1 Collections — section 7.

#### 6.3.2 Identifiers derived from the token [dec. i, Q3]
`person_id = volunteer_id = base64url(HMAC-SHA256(HMAC_KEY, token))` (43 characters). The raw token is not stored anywhere. Uniqueness is still by the document ID [dec. g]. `no_biometrics` people use a client UUID until they gain biometrics (4.12). Reasons: the token format is not documented and may contain `/` (forbidden in a Firestore ID); the token is the same in any Unico client, and the HMAC prevents cross-referencing databases in case of a leak; revalidation compares derived IDs.
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
| `people` | `event_ids` ARRAY_CONTAINS, `level` ASC |
| `households` | `member_ids` ARRAY_CONTAINS |
| `cards` | `household_id` ASC, `status` ASC |
| `deliveries` | `person_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `household_id` ASC, `item` ASC, `delivered_at` DESC |
| `deliveries` | `shelter_id` ASC, `status` ASC, `delivered_at` DESC |
| `searches` | `person_ids` ARRAY_CONTAINS, `status` ASC |
| `searches` | `event_id` ASC, `status` ASC |
| `matches` | `mediator_kind` ASC, `mediator_shelter_id` ASC, `status` ASC, `created_at` DESC |
| `matches` | `mediator_kind` ASC, `status` ASC, `created_at` DESC (`central` and `protection` queues, which have no `mediator_shelter_id`) |
| `memberships` | `volunteer_id` ASC, `status` ASC |
| `memberships` | `shelter_id` ASC, `status` ASC |
| `alerts` | `resolved` ASC, `informational` ASC, `created_at` DESC |
| `audit_log` | `shelter_id` ASC, `at` DESC |
| `audit_log` | `volunteer_id` ASC, `at` DESC |

TTL policies: `sessions.expires_at`, `captures.expires_at`, `idempotency.expires_at`, `audit_log.expires_at`, `search_links.expires_at`.

#### 6.3.5 Free tier quotas (reference)
1 GiB stored; 50,000 reads, 20,000 writes and 20,000 deletes per day. Each operation = 1 business write + 1 audit write.

### 6.4 VM and deployment
The infrastructure is created and configured with Ansible in `infra/ansible/`: `provision.yml` (APIs, Firestore, service account with only `roles/datastore.user`, static IP, firewall 22/80/443, e2-micro VM with Debian 13 and a 30 GB `pd-standard` disk) and `configure.yml` (automatic updates, Caddy, `/etc/taba/env`). The step-by-step guide is in **`infra/README.md`**; this spec does not repeat it.

- **Inventory:** `infra/ansible/inventory/example/` (in git, example values) and `inventory/prod/` (outside git, each installation's values and vault). No installation value (project, IP, keys) enters the repository.
- **Secrets:** on the VM, `/etc/taba/env` (mode 0600, directory 0700) generated by the `env.j2` template from the installation's vault. The API deployment adds to `env.j2` and to `vault.yml.example` the missing variables from 6.2.2, with vault keys `vault_taba_hmac_key` and `vault_taba_thumbnail_key`. In development, `.env` from `.env.example` (which will list all variables, without values).
- **Caddy:** currently responds only with a placeholder (`Caddyfile.j2`). The API deployment replaces it with: `file_server` for the PWA at `/srv/taba/pwa` and `reverse_proxy /v1/* 127.0.0.1:8080`, with the headers `Strict-Transport-Security`, `Content-Security-Policy` (`default-src 'self'; connect-src 'self' https://nominatim.openstreetmap.org; img-src 'self' blob: data:`), `X-Content-Type-Options`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(self), geolocation=(self)`.
- **systemd:** `taba.service` (`ExecStart=/usr/local/bin/taba serve`, `EnvironmentFile=/etc/taba/env`, `Restart=always`, `DynamicUser=yes`), installed by a new task in `configure.yml`.
- **Swap:** 1 GB (the VM has 1 GB of RAM), also through `configure.yml`.
- **Build:** the binary is compiled outside the VM (`x86_64-unknown-linux-gnu`) and copied; the e2-micro does not have enough memory to compile Rust.
- The API trusts `X-Forwarded-For` only when coming from `127.0.0.1`.
- Cost: free tier, except the external IPv4 (~US$ 3/month) [dec. g] and egress above 1 GB/month [dec. i, Q5]. For a real event, the VM sizing needs to be reviewed [dec. k, A12].

---

## 7. Data model (Firestore)

Dates: `Timestamp`. `client_*` = phone time; no prefix = server time.

### `system/status`
`unico: ok|degraded`, `reason?: limit|unavailable`, `since`, `until?`.

### `events/{event_id}` (server UUID)
`name`, `auto_named: bool`, `status: active|closed|purged`, `created_at`, `last_activity_at`, `closed_at?`, `closure_reason?: all_shelters_closed|inactivity`, `purge_at?`, `delivery_purge_at?`, `purged_at?`, `reference_contacts: {civil_defense_phone?, child_protection_phone?}`, `stats?: {shelters, unique_people, households, rescues, deliveries}` (stored at purge).

### `shelters/{shelter_id}` (client UUID)
`name`, `lat`, `lon`, `accuracy_m`, `geohash`, `address: {text, city, state, pending: bool}`, `event_id?`, `capacity?`, `created_by`, `client_created_at`, `created_at`, `status: open|closed`, `confirmation: unconfirmed|confirmed`, `confirmation_type?: validator|in_person`, `can_endorse: bool`, `endorsements: [{kind: in_person, volunteer_id, endorser_shelter_id, at, distance_m}]`, `revocations: [{by, at, reason}]`, `flagged: bool`, `other_site_reason?`, `conflicts_with?`, `items: {<name>: {scope: per_person|per_household, period_days?, once_per_event: bool, high_value: bool}}`.

### `volunteers/{volunteer_id}` (HMAC of the token)
`name`, `phone?`, `show_phone: bool`, `language: pt-BR|en`, `global_roles: [validator|civil_defense|mediator|protection]`, `role_grants: [{role, by, at}]`, `banned: bool`, `banned_by?`, `banned_at?`, `flagged: bool`, `flag_reasons: []`, `created_at`, `last_seen_at`.

### `memberships/{shelter_id}_{volunteer_id}`
`shelter_id`, `volunteer_id`, `role: admin|volunteer`, `status: active|removed|left`, `via: creator|qr|link`, `invite_code?`, `invited_by?`, `joined_at`, `removed_by?`, `removed_at?`.

### `invites/{code}` (128 random bits, base64url)
`shelter_id`, `kind: qr|link`, `role: admin|volunteer` (`link` ⇒ `volunteer`), `created_by`, `created_at`, `expires_at`, `status: active|used|canceled`, `used_by?`, `used_at?`.

### `endorsement_codes/{code}`
`shelter_id`, `created_by`, `lat`, `lon`, `accuracy_m`, `expires_at`, `used: bool`.

### `sessions/{sha256(secret)}`
`volunteer_id`, `device_id`, `created_at`, `renewed_at`, `expires_at` (TTL).

### `captures/{capture_id}` (server UUID)
`process_id` (Unico), `purpose: login|invite|checkin|revalidation|biometrics`, `device_id`, `volunteer_id?`, `shelter_id?`, `expected_person_id?`, `status: created|ok|inconclusive|failed|canceled`, `person_id?` (HMAC; never the token), `consumed: bool`, `created_at`, `expires_at` (15 min, TTL).

### `people/{person_id}` (HMAC of the token; client UUID if `no_biometrics`)
- Identity and level: `level: verified|assisted|no_biometrics`, `biometrics_declined: bool`, `former_ids: []`, `name?`, `social_name?`, `wristband?`, `birth_year?`.
- Child: `is_child: bool`, `child_status?: with_guardian|separated|unaccompanied`, `guardians: [{person_id, link: declared|document_checked, checked_by?, checked_at?}]`.
- Needs and vulnerabilities: `needs: {continuous_medication, dietary_restriction, reduced_mobility, health_follow_up}` (bools), `vulnerabilities: {pregnant, lactating, disability: [], indigenous_people?, migrant_language?}`.
- Family: `household_id?`.
- Stays: `current_shelter_id?`, `stays: [{shelter_id, event_id, entered_at, left_at?, exit_reason?: checkin_elsewhere|exit|exceptional_exit, exit_destination?: own_home|relatives_or_friends|rental_aid|other_shelter|other|unknown, destination_municipality?, registered_by}]`, `event_ids: []`, `rescue_ids: []`.
- Other: `thumbnail_encrypted?: bytes`, `origin: checkin|rescue`, `legal_basis`, `created_at`, `updated_at`, `client_edited_at`.

### `households/{household_id}` (client UUID)
`member_ids: []`, `head_id?`, `pets: [{species, count}]`, `created_by`, `created_shelter_id`, `client_created_at`, `created_at`, `updated_at`.

### `cards/{code}` (8 Crockford base32 characters, generated on the client)
`household_id`, `shelter_id` (issuer), `status: active|canceled`, `issued_by`, `client_issued_at`, `created_at`, `canceled_at?`, `replaced_by?`.

### `deliveries/{delivery_id}` (client UUID)
`shelter_id`, `shelter_name`, `event_id`, `person_id?`, `household_id?`, `household_pseudonym?`, `card_code?`, `item`, `scope`, `quantity`, `delivered_by`, `delivered_at` (client), `received_at`, `verification: card|liveness|admin_approval`, `approved_by?`, `status: ok|in_review|reviewed_ok|irregular`, `reasons: [{kind: duplicate_in_period|liveness_unavailable, previous_delivery_id?}]`, `reviewed_by?`, `review_note?`, `pseudonymized: bool`.

### `rescues/{rescue_id}` (client UUID)
`person_id`, `volunteer_id`, `device_id`, `lat`, `lon`, `accuracy_m`, `landing_point?`, `captured_at` (client), `received_at`, `event_id?`, `destination_shelter_id?`, `note?`.

### `searches/{search_id}` (client UUID)
`created_by` (the volunteer the search is in the name of: who typed it or who made the family link), `via: volunteer|family_link`, `origin_shelter_id?`, `event_id?`, `person_ids: []` (HMAC of the photos' tokens), `photos_sent`, `usable_photos`, `missing_person: {name, nickname?, notes}`, `family: {name, phone, relationship, language}`, `status: active|closed`, `expiry_notified_at?`, `created_at`.

### `search_links/{code}` (128 random bits, base64url)
`created_by`, `shelter_id`, `event_id?`, `status: active|used|canceled`, `created_at`, `expires_at` (12 h; TTL), `used_at?`, `search_id?`, `attempts`.

### `matches/{sha256(search_id|person_id)}`
`search_id`, `person_id`, `is_child: bool`, `mediator_kind: shelter|central|protection`, `mediator_shelter_id?`, `status: open|accepted|declined|not_the_person`, `decided_by?: person|operator`, `decided_by_volunteer?`, `decided_at?`, `note?`, `created_at`.

### `alerts/{alert_id}`
`kind: anomaly_volume|anomaly_distance|out_of_area|volunteer_removed|shelter_flagged|event_overlap|unico_limit|endorser_flagged|exceptional_exit|child_separated|search_expiring|family_search_received|household_conflict|revalidation_failed`, `informational: bool`, `target_kind`, `target_id`, `event_id?`, `shelter_ids: []`, `volunteer_ids: []`, `for_civil_defense: bool`, `for_protection: bool`, `detail`, `created_at`, `resolved: bool`, `resolved_by?`.

### `audit_log/{auto}`
`at`, `volunteer_id?`, `device_id?`, `ip?`, `shelter_id?`, `action`, `target_kind`, `target_id`, `legal_basis?`, `lat?`, `lon?`, `taken_over_from?`, `expires_at` (1-year TTL [dec. k, Q21]). No name and no photo.

### `idempotency/{key}`
`volunteer_id?`, `route`, `http_status`, `body`, `expires_at` (7 days, TTL).

`legal_basis` (enum): `consent` (art. 11, I) | `vital_interest` (art. 11, II, "e") | `fraud_prevention` (art. 11, II, "g") | `public_policy` (art. 11, II, "b") | `child_protection` (art. 14, § 3) | `family_consent` (art. 7, I).

---

## 8. REST API

Base `https://<domain>/v1`. JSON. Authentication `Authorization: Bearer <session>` except where marked "public". Routes that write accept `Idempotency-Key` (the app uses the queue item's `id`) and `X-Device-Id`; a repeated key returns the response stored in `idempotency`. Routes that depend on Unico respond `503 unico_degraded` in degraded mode.

### 8.1 Status, captures and session
| Method and route | Auth | Body | Response |
|---|---|---|---|
| `GET /status` | public | — | `{unico: ok\|degraded, reason?}` |
| `POST /captures` | public for `login` and `invite`; session for the others | `{purpose, shelter_id?, expected_person_id?}` | `201 {capture_id, link, expires_at}` |
| `POST /sessions` | public | `{capture_id, profile?: {name, phone?, show_phone, language}}` | `201 {session, expires_at, volunteer}`; `202 capture_pending`; `422 profile_required`; `403 banned` |
| `DELETE /sessions/current` | session | — | `204` |
| `GET /me` | session | — | `{volunteer, global_roles, memberships: [{shelter_id, name, role, confirmation}]}` |
| `PATCH /me` | session | `{name?, phone?, show_phone?, language?}` | `200` |

### 8.2 Shelters and events
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters` | `{id, name, lat, lon, accuracy, address?, capacity?, other_site_reason?, client_created_at}` | `201 {shelter, event?}`; `409 shelter_nearby {shelter_id, name, distance_m, contact}`; `422 insufficient_accuracy` |
| `GET /shelters/{id}` | — | shelter (active members; other volunteers see only name, confirmation and vacancies) |
| `GET /shelters/nearby?lat&lon` | — (any volunteer) | `{shelters: [{id, name, distance_m, capacity, occupancy, vacancies, confirmation}]}`, nearest first (open, of an active event, within ≤ `EVENT_RADIUS_KM`) |
| `PATCH /shelters/{id}` | `{name?, address?, capacity?}` (admin; DC for `capacity`) | `200` |
| `POST /shelters/{id}/close` | — (admin or DC) | `200` |
| `PUT /shelters/{id}/items` | `{items: {<name>: {scope, period_days?, once_per_event, high_value}}}` (admin) | `200` |
| `PATCH /events/{id}` | `{name?, reference_contacts?}` (`name`: admin of a shelter of the event or DC; `reference_contacts`: Val or DC) | `200` |

### 8.3 Confirmation
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/confirm` | — (Val, DC) | `200` |
| `POST /shelters/{id}/revoke` | `{reason}` (Val, DC) | `200` |
| `POST /shelters/{id}/endorsements/in-person/code` | `{lat, lon, accuracy}` (admin of the new shelter) | `201 {code, expires_at}` |
| `POST /endorsements/in-person` | `{code, endorser_shelter_id, lat, lon, accuracy}` | `200 {shelter}`; `403 cannot_endorse`; `422 too_far {distance_m}` |

### 8.4 Invites, members and roles
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/invites` | `{kind: qr\|link, role}` (admin) | `201 {code, url, expires_at}`; `422 link_volunteer_only` |
| `GET /shelters/{id}/invites` | — (admin) | active ones and who joined via link |
| `DELETE /invites/{code}` | — (admin) | `204` |
| `GET /invites/{code}` | public | `{shelter_name, role, confirmed, valid}` |
| `POST /invites/{code}/accept` | public `{capture_id, profile?}` | `201 {session, membership}`; `410 invite_invalid`; `409 already_member`; `403 banned\|removed_from_shelter` |
| `GET /shelters/{id}/members` | — (member) | list |
| `PATCH /shelters/{id}/members/{volunteer_id}` | `{role: admin}` (admin) | `200` |
| `DELETE /shelters/{id}/members/{volunteer_id}` | — (admin removes volunteer; DC removes anyone) | `204`; `403 admin_cannot_remove_admin`; `409 last_admin` |
| `POST /shelters/{id}/leave` | — | `204`; `409 last_admin` |
| `POST /volunteers/{volunteer_id}/ban` | `{reason}` (DC) | `200` |
| `POST /volunteers/{volunteer_id}/roles` | `{role: mediator\|protection}` (Val, DC) | `200` |
| `DELETE /volunteers/{volunteer_id}/roles/{role}` | — (Val, DC) | `204` |

### 8.5 People, households and cards
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/checkins` | `{id, capture_id? \| photo_base64? \| no_biometrics: true, biometrics_declined?, data: {name?, social_name?, wristband?, birth_year?, is_child, child_status?, needs?, vulnerabilities?}, thumbnail_base64?, legal_basis: consent\|vital_interest\|child_protection, client_created_at}` | `201 {person_id, level, is_new, history}`; `422 no_face\|capture_inconclusive\|invalid_image` |
| `GET /shelters/{id}/people` | — (member) | people with `current_shelter_id = id` (history per 4.11) |
| `GET /shelters/{id}/people/needs-selfie` | — | `assisted` and `no_biometrics` without refusal at the shelter |
| `GET /people/{person_id}` | — (4.11) | person |
| `PATCH /people/{person_id}` | `{data, client_edited_at}` | `200`; deletes the thumbnail if `name` or `wristband` come filled in; `child_status` only by `protection` or DC |
| `PUT /people/{person_id}/biometrics` | `{capture_id? \| photo_base64?}` | `200 {person_id, level}`; `422 no_face\|capture_inconclusive` |
| `POST /people/{person_id}/guardians` | `{guardian_id, link: declared\|document_checked}` | `200`; `422 guardian_not_verified` |
| `GET /people/{person_id}/thumbnail` | — (4.6) | `image/jpeg`, audited |
| `POST /shelters/{id}/exits` | `{person_id, exit_destination, destination_municipality?, guardian_id?, capture_id?, exception?: {reason}}` | `200`; `409 revalidation_failed`; `403 protection_role_required` |
| `POST /shelters/{id}/households` | `{id, member_ids, head_id?, pets?, client_created_at}` | `201`; `409 person_in_other_household` |
| `GET /households/{id}` | — (member of a shelter where some member is) | `{id, members: [{person_id, name, level}], head_id, pets, card_code?}` |
| `PATCH /households/{id}` | `{add_member_ids?, remove_member_ids?, head_id?, pets?}` | `200`; `409 person_in_other_household` |
| `POST /households/{id}/cards` | `{code, client_issued_at}` | `201`; `409 code_taken\|household_has_card` |
| `GET /cards/{code}` | — (member of any shelter) | `{code, status, household_id, members: [{person_id, name, level}]}` (no current shelter or history) |
| `POST /cards/{code}/cancel` | `{replaced_by?}` | `200` |

### 8.6 Deliveries
| Method and route | Body | Response |
|---|---|---|
| `POST /shelters/{id}/deliveries` | `{id, card_code, person_id, item, quantity, delivered_at, capture_id?, admin_approval?: true}` | `201 {delivery_id, status, reasons}`; `428 liveness_required`; `409 revalidation_failed`; `403 admin_approval_required`; `404 card_not_found\|person_not_in_household` |
| `GET /shelters/{id}/deliveries?status=` | — (admin) | list |
| `POST /deliveries/{id}/review` | `{decision: reviewed_ok\|irregular, note}` (admin) | `200` |

In the response of `POST /shelters/{id}/deliveries`, each item of `reasons` carries `kind`, `previous_delivery_id?`, `previous_delivered_at?` and `previous_shelter_name?` (per the rule of item 6 of 3.5.3, 4.7). `PUT /shelters/{id}/items` answers `{items}`; `GET /shelters/{id}/deliveries` answers `{deliveries: [...]}` with names (`person_name`, `delivered_by_name`, `approved_by_name`), never the `person_id`.

Delivery details: the body accepts `offline: true` (a delivery made without signal or with Unico out: no draw; `high_value` without `admin_approval` becomes `in_review` with `liveness_unavailable`; the server applies the same rule in degraded mode). `428 liveness_required` carries `detail: {reason: high_value|sample, admin_approval_only}`, and `admin_approval_only` is `true` for a person without biometrics (a selfie cannot settle it, only the admin). Sending the same `id` again with the same volunteer and shelter returns the same answer; another volunteer or shelter gets `409 delivery_id_taken`. A revalidation that does not match spends the capture, writes no delivery and audits `delivery.revalidation_failed`. Duplicates consider deliveries less than `period_days` apart, in any order (the offline queue can arrive out of order). A delivery that arrives with an old `person_id` (late biometrics, 4.12) is for the member of the household who has it in `former_ids`. The audit (`delivery.create`, `delivery.admin_approval`, basis `fraud_prevention`) keeps the `volunteer_id` of the sender (the admin, in an approval); the name comes from the volunteer's record. `GET /shelters/{id}/deliveries` requires `status`. `GET /shelters/{id}` carries `items` for members, validators and the Civil Defense. `delivered_at` (the phone's clock) older than 30 days is refused (`422 invalid_request`, `detail.field = delivered_at`); a time in the future is not refused, so that a phone with its clock ahead does not stall the queue: the server cuts it to the time of receipt and records `delivered_at_clamped: true` (the audit action gets the suffix `_clamped`). The 30 s cut of `offline` and the duplicate check use the value already cut. `admin_approval` counts only where a selfie would be asked (`high_value` or drawn, or `high_value` offline or in degraded mode); otherwise the delivery is an ordinary one (`verification = card`, no `approved_by`). `previous_delivered_at` and `previous_shelter_name` follow the rule of item 6 of 3.5.3. `POST /deliveries/{id}/review` only decides a delivery `in_review` (`409 delivery_not_in_review`, `404 delivery_not_found`).

### 8.7 Rescues, searches and matches
| Method and route | Body | Response |
|---|---|---|
| `POST /rescues` | `{id, photo_base64?, lat, lon, accuracy, captured_at, data?: {name?, wristband?, note?}, landing_point?, destination_shelter_id?, thumbnail_base64?}` | `201 {rescue_id, person_id, level}` |
| `PATCH /rescues/{id}` | `{landing_point?, destination_shelter_id?}` (author) | `200` |
| `POST /searches` | `{id, photos_base64: [1..5], missing_person: {name, nickname?, notes}, family: {name, phone, relationship, language}}` | `202 {message: "search_registered", usable_photos}` (same with or without a match); `429 search_limit` |
| `GET /matches` | — | queues the volunteer handles (4.8) |
| `GET /matches/{id}` | — | details for checking, without the family's contact |
| `POST /matches/{id}/decision` | `{decision: accept\|decline\|not_the_person, decided_by: person\|operator, note?}` | `200 {family?, whatsapp_link?}`; `409 match_already_decided` |

### 8.8 Dashboard, alerts, export
| Method and route | Auth | Response |
|---|---|---|
| `GET /dashboard/events` | Val, DC | events with counts |
| `GET /dashboard/events/{id}` | Val, DC | 3.10 |
| `GET /dashboard/events/{id}/daily.csv` | Val, DC | daily CSV for the bulletin (3.10) |
| `GET /alerts` | admin (of their shelters, non-informational), `protection` (protection alerts), DC (all) | list |
| `POST /alerts/{id}/resolve` | recipient | `200` |
| `GET /shelters/{id}/export.csv` | admin of a `confirmed` shelter | CSV of the people, households and deliveries of the own shelter, without thumbnail or `person_id`, audited [dec. k, Q22] |

---

## 9. Offline synchronization and conflicts

### 9.1 What works offline
Create shelter, check-in by photo (`assisted`) or without biometrics, household and card, editing a person's data, adult exit, delivery by card, rescue, search [dec. k, Q45], address update, operator switch by PIN. **Does not work offline:** login, invites, captures with liveness (includes revalidated high-value delivery and child exit), endorsement, confirmation, mediation, dashboard.

### 9.2 Queue
- Each write becomes an item in `queue` with a UUID `id`, used as the `Idempotency-Key` and, when the entity is new, as the document ID.
- Draining: when the app opens, on the `online` event, right after enqueuing and every 30 s while there are pending items and the app is open (iOS does not have Background Sync).
- FIFO, one item at a time. By result:
  - `2xx` or idempotent response: removes the item and the encrypted `Blob`; updates caches.
  - `401`: stops draining and asks for login. Items of another volunteer only proceed with that volunteer's session (switch by PIN) or if an admin takes them over.
  - **Taking over items** [dec. k, M1]: an admin of the shelter, with an active session on the device, taps "Assumir pendências de <nome>" ("Take over <name>'s pending items"). The app changes the items' `volunteer_id` and records `taken_over_from`; the server writes the audit record with that field.
  - `409 shelter_nearby` on creation: the item and all those with the same `shelter_id` become `held` until the admin explains (3.2.1).
  - Image/face `422`: `needs_action`; the app lists it in "Pendências" (Pending issues).
  - `503 unico_degraded`: the item stays `pending` and is only retried when `GET /status` returns to `ok`.
  - `429`, `5xx`, `503`, network: `attempts += 1`, `next_attempt_at = now + min(5 s × 2^attempts, 10 min) × (0.5 to 1.5 random)`; respects `Retry-After`.
  - Other `4xx`: `needs_action` with the code.
- "N pendentes" ("N pending") counter always visible; warning if there is an item pending for more than 24 h.

### 9.3 Dependencies
Items that reference a shelter, person or household created offline come after it (FIFO). If the shelter is `held`, the dependents are held too.

### 9.4 Conflict resolution (the server is the source of truth [dec. a])
| Case | Rule |
|---|---|
| Same person registered on two phones (offline) | Same token → same `person_id`. The second check-in becomes an update: adds a `stay`; level only goes up. |
| Same person at two shelters at the same time | The `stay` with the most recent `entered_at` becomes `current_shelter_id`; the other closes with `exit_reason = checkin_elsewhere`. Entries less than 1 h apart and shelters > 50 km apart: `anomaly_distance` alert. |
| Same person in two households created offline | The first to sync stays; the second receives `409 person_in_other_household` → `needs_action` ("pessoa já está no núcleo X" ("person is already in household X")). |
| Two cards for the same household | The second receives `409 household_has_card` → `needs_action`. |
| `no_biometrics` record that gains the token of an already registered person | 4.12. |
| Editing a person's data | Last to edit wins, by `client_edited_at`; an older edit is ignored (`200` with the current state). |
| Repeated item (retry) | `Idempotency-Key` returns the original response; no new call to Unico. |
| Two shelters < 100 m apart created offline | 3.2.1 (pilot: explanation and Civil Defense). |
| Two events reached by the same shelter | 4.3 (pilot: `event_overlap` alert). |
| Write to a shelter of a closed event | `409 event_closed` → `needs_action` [dec. k, Q36]. |

---

## 10. Security and privacy

### 10.1 LGPD

#### 10.1.1 Legal basis per operation (stored in `audit_log.legal_basis`)
| Operation | `legal_basis` |
|---|---|
| Check-in of a conscious adult (with or without selfie) | `consent` (art. 11, I) |
| Check-in of an injured/incapacitated person, rescue, thumbnail | `vital_interest` (art. 11, II, "e") |
| Child check-in and exit, child alerts | `child_protection` (art. 14, § 3) |
| Donation delivery and revalidation | `fraud_prevention` (art. 11, II, "g") |
| Missing person search and mediation | `vital_interest` |
| Family contact data | `family_consent` (art. 7, I) |
| Volunteer accreditation | `fraud_prevention` [dec. k, Q35] |
| Civil Defense dashboard and operation | `public_policy` (art. 11, II, "b") |

#### 10.1.2 Differentiated purge [dec. k, A10, Q9, A5]
- **Event closed:** deletes thumbnails immediately [dec. e]; warns whoever registered active searches without a match (3.7).
- **`purge_at = closed_at + PURGE_AFTER_DAYS` (30):** deletes people whose events in `event_ids` are all past due (including the IDs derived from the token and the `no_biometrics` records), households, cards, rescues, searches (with the derived `person_ids`), matches, alerts, memberships and shelters of the event. **Deliveries** are pseudonymized: `person_id` and `card_code` are deleted, and `household_id` becomes `household_pseudonym` (a random value per household, with no link to a person); shelter (name), item, scope, quantity, date and status remain.
- **`delivery_purge_at = closed_at + DELIVERY_RETENTION_DAYS`:** deletes the pseudonymized deliveries. In the pilot, 90 days [dec. l, Q50]; once there is a controller, the accountability period becomes theirs.
- **Event:** keeps only `name`, dates and `stats`, `status = purged`.
- **Volunteer profiles:** deleted 365 days after `last_seen_at`, except banned ones [dec. k, Q21].
- **Audit:** 1-year TTL [dec. k, Q21].
- **Photos of people** never stay on the server: they only exist in the process memory during the call to Unico [dec. e]. On the phone, they are kept encrypted and deleted after the `2xx` (6.1.7).

#### 10.1.3 Transparency [dec. k, C1, M2]
- Short text on each collection screen (check-in, search): purpose, retention period, who has access, legal basis, who is responsible.
- Responsible party: in the pilot there is no defined controller [dec. k, Q10]; the screens show `PILOT_RESPONSIBLE`, the person responsible for the pilot defined in the installation [dec. l, Q51].
- Mandatory sentences at check-in: "Recusar a selfie não tira abrigo nem doação" ("Refusing the selfie does not take away shelter or donation") and "Os dados não são compartilhados com polícia nem com imigração" ("The data is not shared with the police or with immigration").
- Short version for reading aloud. Consent in PT, EN and ES [dec. h, M3].
- On search: "Esta busca não substitui o registro de desaparecimento na delegacia" ("This search does not replace the missing person report at the police station") [dec. k, A5].
- Page `/privacy` with the full text.

#### 10.1.4 RIPD, region and international transfer
- The repository must contain `docs/pt_br/RIPD.md` before use with real data (art. 38): data processed (ID derived from the token, thumbnail, name, social name, vulnerabilities, operational needs, household, location, card, family contact), purposes, legal bases (10.1.1), risks (abuser, fake shelter, malicious operator, leak, match false positive, coercion to consent), measures (this section), purge.
- Region [dec. h, A8]: the pilot is in the US (`us-east1`). The RIPD records the international transfer (art. 33) and documents that each controller can bring up its own instance in `southamerica-east1` (São Paulo), at the cost of leaving the free tier, with no code change: only the Ansible inventory changes.
- Controller and art. 33 mechanism: defined when there is use with a real organization [dec. k, Q10, A9].

### 10.2 Protection against fake shelters
- Born `unconfirmed`: sees only what it registered, does not search, does not mediate, does not export [dec. d].
- The consent screen shows "Abrigo não confirmado pela Defesa Civil" ("Shelter not confirmed by Civil Defense").
- Creation requires a profile with liveness: the creator is identifiable and bannable.
- In the pilot, only a validator or an in-person endorsement confirms [dec. k]. Revocation flags endorsers.
- Shelter < 100 m from another: blocked or `flagged` for Civil Defense.

### 10.3 Protection against an abuser looking for the victim
- Search only by an operator of a confirmed shelter, Civil Defense, `mediator` or `protection`; daily limit per volunteer [dec. k, Q8, A5].
- Search response identical with or without a match.
- Whoever mediates is from the shelter where the person is, `mediator`, `protection` or Civil Defense — never the shelter of whoever searched.
- The mediator checks the data with the person before accepting; a false positive becomes `not_the_person` [dec. k, A6].
- The family's contact only appears after the person accepts, with the instruction to delete it afterwards [dec. k, A4].
- Location is never sent automatically.
- An ordinary volunteer does not see which shelters the person has been through [dec. k, M6].
- Card and household reading returns only the members' names and level, with no current shelter or history.
- Search writes an audit record with who searched.
- Residual risk: an abuser who is an admin of the same shelter as the victim. Mitigation: a `mediator` can handle the shelter's queue (4.8), and audit; recorded in the RIPD.

### 10.4 Public endpoints and cost
- Public in the pilot: `GET /status`, `POST /captures` (only `login` and `invite`), `POST /sessions`, `GET /invites/{code}`, `POST /invites/{code}/accept`.
- Limits on public captures stay with section 16 (Q15, postponed). In the pilot, abuse only consumes credits, and running out of credits leads to degraded mode [dec. k, Q7].
- Maximum body of 6 MB (5 photos of up to 800 KB in base64); individual image ≤ 800 KB.
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
- WhatsApp message to the family: in the `family.language` given in the search (`pt-BR`, `en` or `es`).
- Dates and numbers: `Intl.DateTimeFormat` and `Intl.NumberFormat`.
- Typed data is not translated. Server errors arrive as stable codes and are translated by the app.

### 11.2 What stays in English
Identifiers, routes, logs, commits and PRs (section 0) [dec. j].

### 11.3 Spanish [dec. h, M3; dec. k, Q43]
In v1, only the check-in consent screen (including the short version for reading aloud). It has its own "PT | EN | ES" selector, independent of the volunteer's language (whoever reads the consent is the person served); the default is the interface language. The "Estou Seguro" page in Spanish comes together with it in the later phase (16.1).

---

## 12. Open questions

**There are no pending items for the owner in this version.** Q33 and Q46–Q51 were closed in item l (section 13). The questions postponed with the post-pilot scope (Q15, Q20, Q29, Q30, Q31, Q40) are in section 16 and come back when those phases are planned.

---

## 13. Decision log

### 13.1 Owner decisions
| Decision | Origin | Where |
|---|---|---|
| Offline check-in is born `assisted` and is upgraded on the first capture with liveness | Q1 [dec. i] | 3.4, 4.1 |
| Shelter merge as a request accepted by the destination (postponed until after the pilot) | Q2 [dec. i, k] | 16.3 |
| HMAC of the token, never the raw token; `HMAC_KEY` in the vault, with backup | Q3 [dec. i] | 6.3.2, 10.5 |
| Liveness through the journey; plan B web SDK | Q4 [dec. i] | 5.1 |
| Egress above 1 GB/month is a known cost | Q5 [dec. i] | 6.1.4, 6.4 |
| Unico's terms allow a photo that did not come from the SDK | Q6 [dec. k] | 5.1 |
| Credits run out: calls stop, the app continues in degraded mode | Q7 [dec. k] | 5.5, 5.7 |
| Search by an operator of a confirmed shelter, Civil Defense, `mediator` or `protection`; 5 photos; 10 per volunteer per day; notice that it does not replace the police report | Q8 + A5 [dec. k] | 3.7 |
| Differentiated purge: biometrics and searches in 30 days; pseudonymized deliveries for the controller's period | Q9 + A10 [dec. k] | 10.1.2 |
| Pilot with no defined controller | Q10, A9 [dec. k] | 1.1, 10.1.3, 10.1.4 |
| AGPL-3.0 license | Q11 | `LICENSE` |
| Public repository `github.com/thingsbuilder/taba` | Q12 | — |
| First task: journey test on iPhone; then login → create shelter → check-in | Q13 [dec. k] | 5.1 |
| Domain `taba.thingsbuilder.com`; project Taba; region `us-east1` | Q14 [dec. g] | 6, 6.4 |
| Duplication per person or per household, depending on the item | Q16 + A2 [dec. k] | 3.5.2, 4.7 |
| Period per item, including "once per event" | Q17 [dec. k] | 3.5.2 |
| Child exit: failed revalidation blocks; exception only by `protection` or Civil Defense | Q18 + C2 [dec. k] | 3.8 |
| No push; child alert shows the reference phone number | Q19 [dec. k] | 3.4.1, 4.9 |
| Audit for 1 year; profile deleted 1 year after last access, except banned ones | Q21 [dec. k] | 10.1.2 |
| No search by name across the network; export only of the own shelter, without `person_id` | Q22 [dec. k] | 4.11, 8.8 |
| `validator` and `civil_defense` via CLI | Q23 [dec. k] | 2.1 |
| Person at an unconfirmed shelter or without a shelter → central queue (`mediator` and Civil Defense) | Q24 + A4 [dec. k] | 4.8 |
| Search active until the purge; notice to whoever registered it before deleting | Q25 + A5 [dec. k] | 3.7 |
| 7-day session, renewed on each use | Q26 [dec. k] | 3.1 |
| Per-shift selfie out of v1 | Q27 [dec. k] | 1.3 |
| `no_biometrics` as a first-class path | Q28 + C1 [dec. k] | 3.4, 4.1 |
| No Firestore backup in the pilot | Q32 [dec. k] | 6.3 |
| Photos for token up to 800 KB, not 300 KB | Q34 changed [dec. k] | 6.1.4 |
| Legal basis for accreditation: art. 11, II, "g" | Q35 [dec. k] | 10.1.1 |
| Shelter of a closed event does not accept writes | Q36 [dec. k] | 4.3 |
| A removed volunteer only returns via in-person QR | Q37 [dec. k] | 3.3 |
| In-person endorsement code expires in 10 min | Q38 [dec. k] | 3.9 |
| Anomaly thresholds, informational only in the pilot | Q39 [dec. k] | 4.10 |
| Liveness sampling on 5% of ordinary deliveries | Q41 [dec. k] | 3.5.3 |
| `assisted` person receives by card; selfie offered, not required | Q42 rejected [dec. k] | 3.4, 3.5.3, 4.1 |
| Own PT/EN/ES selector on the consent screen | Q43 [dec. k] | 11.3 |
| 8-character card code; card belongs to the household, valid across the network | Q44 + A3 [dec. k] | 3.5.1 |
| Offline search through the queue, photos encrypted and deleted after sending | Q45 [dec. k] | 3.7, 6.1.7 |
| "I need rescue" becomes 193/199 without storing an alert | C3 [dec. h] | 16.1 |
| Checking at check-in; delivery by card; liveness for high value and by sampling | A1 [dec. h] | 3.5 |
| Rescue at the landing point | A11 [dec. h] | 3.6 |
| US in the pilot; region per installation | A8 [dec. h] | 6, 10.1.4 |
| Consent in Spanish | M3 [dec. h] | 11.3 |
| Refusing the selfie does not take away shelter or donation; refusal rate on the dashboard | C1 [dec. k] | 3.4, 3.10 |
| Child `with_guardian`/`separated`/`unaccompanied`; link `declared`/`document_checked`; alert; protection queue | C2 [dec. k] | 3.4.1, 3.8, 4.8, 4.9 |
| Household as an entity; card belongs to the household; animals as a count | A3 [dec. k] | 3.5.1, 7 |
| `mediator` role; admin is the default mediator; instruction to delete the contact | A4 with the owner's adjustment [dec. k] | 2.1, 4.8 |
| False positive: checking and `not_the_person` decision | A6 [dec. k] | 4.8 |
| Dashboard in Civil Defense's format: capacity, vacancies, breakdowns, exits by destination, daily CSV, homeless x displaced | A7 [dec. k] | 3.10 |
| Degraded mode | A12 [dec. k] | 5.7 |
| Admin takes over pending items; operator switch by PIN | M1 [dec. k] | 3.1.1, 9.2 |
| Social name; structured vulnerabilities; consent read aloud; sentence about police and immigration | M2 [dec. k] | 3.4, 10.1.3 |
| Needs as a closed list | M4 [dec. k] | 3.4, 7 |
| Destination at exit | M5 [dec. k] | 3.8, 3.10 |
| History from other shelters only for admin | M6 [dec. k] | 4.11 |
| Count "other site" justifications to calibrate the radius | M7 [dec. k] | 3.2, 3.10 |
| Gaps: capacity, protection role, homeless x displaced, destination, reference contacts per event, animals | Section 3 of the review [dec. k] | 2.1, 3.10, 7 |
| CadÚnico, health and Red Cross/RFL out of v1 | [dec. k] | 1.3 |
| Coming in now: Nominatim and offline search | [dec. k] | 6.1.6, 3.7 |
| For after the pilot: remote endorsement, merges, site photos, "Estou Seguro" | [dec. k] | 16 |
| Postponed with that scope: Q15, Q20, Q29, Q30, Q31, Q40 | [dec. k] | 16 |
| Implementation, logs, commits and PRs in English | [dec. j] | 0 |
| High-value or sampled delivery without a selfie: the admin checks against the card and approves on the spot, with audit; no review queue | Q46, option (c) [dec. l] | 3.5.3, 4.7 |
| Whoever decides first closes the match (admin or `mediator`); `mediator` applies to the whole network | Q47 [dec. l] | 4.8 |
| `protection` granted by Civil Defense or a validator, like `mediator` | Q48 [dec. l] | 2.1 |
| Human Token works for children; thumbnail for babies and unidentified children | Q49 [dec. l] | 3.4.1 |
| Pseudonymized deliveries kept for 90 days in the pilot | Q50 [dec. l] | 6.2.2, 10.1.2 |
| With no controller, the screens show the person responsible for the pilot (`PILOT_RESPONSIBLE`) | Q51 [dec. l] | 10.1.3 |
| Thumbnail of a rescued person with no shelter: the landing point volunteer who registered them and Civil Defense | Q33 [dec. l] | 4.6 |
| "Acolhida" design system, with a monospaced font for codes and a dark theme; tokens in `design/` | [dec. l] | 6.1 |

### 13.2 Technical decisions
| Decision | Reason |
|---|---|
| Revalidation by comparing derived IDs | Same result and cost as the `+revalidation` flow, without storing the raw token. Rejected `humantoken+revalidation`: it would require the raw token in `references`. |
| Global in-memory lock for shelter creation | One instance; solves the 100 m race. Rejected: transaction with a geographic query (9 queries) and a per-cell lock document. |
| Hourly job inside the process | One less systemd unit. Rejected systemd timer/cron. |
| Generic idempotency via `Idempotency-Key` | The queue only resends HTTP requests; no repeated (paid) call to Unico. Rejected a batch `/sync` endpoint. |
| Client-generated IDs for entities created offline | Reference between queue items before sync, and natural deduplication. |
| Late biometrics migrates the record to the ID derived from the token | Keeps "uniqueness by document ID" [dec. g] and still allows registering without biometrics (C1, A12). Rejected: own ID + token index, which would change dec. g. |
| Degraded mode with reason `limit` only ends by operator command | Every call to Unico is paid; there is no free probe. |
| Queue photos encrypted with a non-extractable WebCrypto key | One rule for all pending photos, not just search; no extra password for the volunteer. |
| Local PIN with PBKDF2 | Offline operator switch without the server; the PIN never leaves the device. |
| Match decision in a transaction that requires `status = open` | Implements "whoever decides first closes it" [dec. l, Q47] without manual assignment. |
| Two events reached: joins that of the nearest shelter and alerts | The pilot's "detection and warning" without merge [dec. k]. |
| Deterministic liveness draw from the `delivery_id` | Retrying the same delivery does not change the draw. |
| Short, typeable card code | Works with an improvised wristband and without a good camera. |
| Search responds the same with or without a match | Even "not found" is information for an abuser. |
| Geohash with 9 cells + haversine | Required by dec. g; simple range queries. |
| Preact + `idb` + `vite-plugin-pwa`; `BarcodeDetector` with `jsQR` as fallback | Small PWA; Safari on iOS does not have `BarcodeDetector`. Rejected React (weight). |
| Mediation via `wa.me` message | No paid SMS/e-mail service. |
| Nominatim called by the phone | Allowed by dec. g; avoids egress on the VM. |
| Infra only through the existing playbooks | `provision.yml`/`configure.yml` already create the VM, Firestore and Caddy; the deployment only adds tasks. |

---

## 14. Repository file structure

```
api/                          Rust (cargo), binary `taba`
  Cargo.toml
  src/main.rs config.rs auth.rs unico.rs geo.rs shelters.rs events.rs members.rs
      people.rs households.rs deliveries.rs rescues.rs searches.rs
      dashboard.rs jobs.rs audit.rs idempotency.rs
  tests/                      integration with fake Unico and the Firestore emulator
pwa/                          Vite + TypeScript + Preact
  src/main.tsx routes.ts api.ts queue.ts db.ts crypto.ts camera.ts image.ts geo.ts nominatim.ts qr.ts
  src/screens/*.tsx
  src/i18n/pt-BR.json en.json es.json
infra/                        already exists
  README.md
  ansible/playbooks/provision.yml configure.yml   (configure gains: service, swap, deploy)
  ansible/templates/Caddyfile.j2 env.j2           (Caddyfile gains reverse_proxy; env.j2 gains variables)
  ansible/templates/taba.service.j2               (new)
  ansible/inventory/example/                      (in git)
  firestore/firestore.indexes.json                (new)
design/                       design system tokens.json and tokens.css (6.1)
.env.example                  all variables from 6.2.2, without values
docs/en/spec/spec.md          this file (English translation)
docs/pt_br/spec/spec.md       original (Portuguese; prevails in case of divergence)
docs/pt_br/RIPD.md            before real data
```

Commits and pull requests in English [dec. j].

---

## 15. Acceptance criteria

Automated tests use a fake Unico server (`UNICO_URL`) and the Firestore emulator. No automated test calls the real Unico.

**Unico and degraded mode**
1. `extract_token` returns `None` for missing `value`, `""` and `"  "`; none of these creates a person or volunteer.
2. `429` with "monthly process limit" is not retried, writes `system/status` as `degraded`/`limit` and generates an `unico_limit` alert; `429` without that message is retried up to 4 times and then becomes `503 unico_busy` with `Retry-After`.
3. In `limit` mode, no call to Unico is made until `taba unico resume`; dependent routes respond `503 unico_degraded`.
4. In degraded mode, check-in without biometrics, household, card and ordinary delivery work; a `high_value` delivery uses the admin approval or, with no admin on the device, goes to `in_review` with `liveness_unavailable`.
5. Image > 800 KB → `413 image_too_large` without calling Unico; the app produces photos below 800 KB.
6. A capture is consumed only once and only by the `device_id` that created it.
7. (Manual, first task) A journey opened from the PWA installed on iPhone returns via the `callbackUri` and produces a session.

**Shelter and event**
8. `accuracy` 101 m → `422 insufficient_accuracy`.
9. Second shelter 80 m from an `open` one → `409 shelter_nearby` with the creator's first name and no phone if `show_phone = false`; with `other_site_reason` → created `flagged`, `shelter_flagged` alert and a count on the dashboard.
10. A shelter 40 km from a shelter of an active event joins it; at 60 km it creates a new event; within ≤ 50 km of shelters of two events it joins that of the nearest shelter and generates `event_overlap`, without merging.
11. An event with no writes for 30 days becomes `closed`, with `purge_at`; thumbnails deleted; active searches without a match generate `search_expiring` for whoever registered them.
12. Two simultaneous creations 50 m apart: only one is created without conflict.
13. There are no routes for shelter merge, remote endorsement, site photos or "Estou Seguro" in the pilot.

**Confirmation**
14. In-person endorsement with phones 150 m apart → `422 too_far`; endorser who is an active member of the new shelter → `403 cannot_endorse`.
15. Revocation returns the shelter to `unconfirmed` and flags the endorsers.
16. `unconfirmed` shelter: `POST /searches` → `403`; export → `403`; check-in history without `stays` from other shelters.

**Invites, members and roles**
17. Link with `role = admin` → `422 link_volunteer_only`.
18. QR used twice → second one `410`; QR at 31 min → `410`.
19. Admin removing admin → `403`; last admin leaving → `409 last_admin`.
20. Banned volunteer does not create a session (`403 banned`) nor accept an invite.
21. Someone removed from a shelter does not return via link (`403 removed_from_shelter`), but does return via QR.
22. Only Val or DC grant `mediator`; an ordinary volunteer receives `403`.

**People, households, cards, deliveries, rescues**
23. Same token in two check-ins → one document in `people`, two `stays`, one open.
24. Offline check-in produces `assisted`; a later capture with liveness → same document, `verified`.
25. Refusing the selfie produces `no_biometrics` with `biometrics_declined = true`; the person joins a household, gets a card and receives an ordinary delivery (`201`, `verification = card`).
26. `PUT /people/{uuid}/biometrics` with a photo creates `people/{hmac}`, repoints household and deliveries, deletes the old record and stores `former_ids`; if the `hmac` already existed, it merges the `stays`.
27. An `assisted` person receives an ordinary delivery by card without `428`.
28. Ordinary delivery by card, not sampled → `201` with `verification = card`, with no call to Unico.
29. Delivery of a `high_value` item without `capture_id` → `428 liveness_required`; with a capture of another person → `409 revalidation_failed` and nothing stored; with `admin_approval: true` sent by an admin of the shelter → `201`, `status = ok`, `verification = admin_approval`, `approved_by` and an audit record with the admin's name; sent by a volunteer → `403 admin_approval_required`.
30. The same delivery resent (same `delivery_id`) has the same draw result.
31. A `per_household` item delivered to two members of the same household within the period → the second becomes `in_review` with `duplicate_in_period`; a `per_person` item does not.
32. A `once_per_event` item delivered again in the same event, even after `period_days`, → `in_review`.
33. Offline delivery of a `high_value` item with no admin session on the device → `in_review`, reason `liveness_unavailable`; with the admin's approval → `ok`.
34. Person in two households → `409 person_in_other_household`; second active card of the household → `409 household_has_card`.
35. `GET /cards/{code}` and `GET /households/{id}` do not return the current shelter or history.
36. A volunteer of a `confirmed` shelter sees only `has_other_stays`; an admin sees the `stays`.
37. Adult exit without `exit_destination` → `422`; the dashboard counts exits by destination.
38. `needs` only accepts the four boolean fields; free text → `422`.
39. Filling in `name` of a person with a thumbnail deletes `thumbnail_encrypted` in the same write.
40. `GET /people/{id}/thumbnail` by a volunteer of another shelter → `403`; permitted access writes an audit record.
41. 50 rescues by the same volunteer in 10 min do not generate `anomaly_volume`; 50 check-ins do, with `informational = true` and outside the admin's "Avisos" (Notices) box.

**Child**
42. Check-in of an `unaccompanied` child generates `child_separated` for protection, Civil Defense and admins, with the phone numbers from `reference_contacts`.
43. Exit of a `separated` child by an admin → `403 protection_role_required`; by `protection`, with revalidation ok, → `200`.
44. Exit of a `with_guardian` child with a `declared` link by a volunteer → `403 protection_role_required`; with `document_checked` and revalidation ok → `200`.
45. Revalidation failed → `409`; exit by exception by an admin → `403`; by `protection` or Civil Defense → `200` and `exceptional_exit` alert.
46. A child's match goes to `mediator_kind = protection` and does not appear to the shelter's admins.

**Search and mediation**
47. `POST /searches` returns an identical body with and without a match; the same volunteer's 11th search of the day → `429 search_limit`.
48. Search queued offline: the photo stays encrypted in IndexedDB (not readable as JPEG) and is deleted after the `2xx`.
49. A match of a person at a `confirmed` shelter appears to that shelter's admins and to `mediator`; the family's contact only comes with `decision = accept`; a second decision → `409 match_already_decided`.
50. `not_the_person` closes the match, keeps the search active and does not create a new match for the same `person_id`.
51. A later check-in of a searched-for person creates a match; repeating it does not create a second match.

**Dashboard**
52. The dashboard counts `assisted` and `no_biometrics`, shows capacity, occupancy and vacancies, breakdowns by age group and vulnerability, exits by destination, refusal rate and the notice about displaced people.
53. `daily.csv` has one row per shelter and contains no name or `person_id`.

**Offline and synchronization**
54. With the network off: create a shelter, 3 check-ins by photo, 1 without biometrics, 1 household with a card, 2 deliveries, 1 rescue and 1 search; turn it back on: everything syncs in order, the `Blob`s are deleted and the counter returns to zero.
55. Resending the same queue item does not produce a second call to Unico.
56. An item with `401` is not resent with another volunteer's session; after "Assumir pendências" (Take over pending items) by an admin, it is sent and the audit records `taken_over_from`.
57. Operator switch by PIN works offline; five wrong PINs delete that volunteer's session on the device.

**Languages**
58. Phone in `pt-PT` opens in pt-BR; in `es` it opens in English, except the consent screen, which opens in Spanish; a manual choice persists after reloading and, when logged in, on another device.

**Security and implementation**
59. No API response contains the raw Unico token; no Firestore document contains the raw token or a photo of a person (only the encrypted thumbnail).
60. After the purge, deliveries have no `person_id` or `card_code`; the event's people, households and searches no longer exist.
61. The Unico key, `HMAC_KEY` and `THUMBNAIL_KEY` do not appear in any file of the repository or of the compiled PWA; the repository does not contain a GCP project ID or IP.
62. Routes, collections, fields, enums, environment variables and log messages are in English (code review).

---

## 16. Phases after the pilot

Design kept for after the pilot [dec. k]. Nothing in this section is built in v1; the questions postponed with this scope live here.

### 16.1 "Estou Seguro" [dec. k; design: dec. h, C3, M3]
Built after the core (shelter, check-in, donation, search).
1. Public page `/safe`, no login, in PT | EN | ES [dec. h, M3].
2. At the top, always visible: **"Precisa de resgate? Ligue 193 (Bombeiros) ou 199 (Defesa Civil)"** ("Need rescue? Call 193 (Fire Department) or 199 (Civil Defense)"), with the buttons `tel:193` and `tel:199`. Nothing is stored through this path [dec. h, C3]. The "I need rescue" alert only comes back when an operations center commits to monitoring the queue.
3. The person reads the notice (purpose, period, legal basis `consent`) and does a capture with liveness (`purpose: "safe_check"`).
4. Chooses "estou seguro" ("I'm safe") (`safe`) or "fui para um abrigo" ("I went to a shelter") (`went_to_shelter`, with an optional list of nearby shelters). GPS optional.
5. Prior consent option: "Se alguém da minha família me procurar, pode saber que estou seguro (sem dizer onde estou)" ("If someone from my family looks for me, they may know I am safe (without saying where I am)") (`family_contact_allowed`) [dec. k, Q24].
6. `POST /v1/safe-check {capture_id, status, reported_shelter_id?, lat?, lon?, family_contact_allowed, language}` creates or updates `people/{person_id}` with `level = verified` and the block `safe_check: {status, reported_shelter_id?, lat?, lon?, family_contact_allowed, at}`; `origin = safe_check`.
7. Mediation: with `family_contact_allowed`, `mediator_kind = search_origin` (admins of the shelter that registered the search receive only "declarou-se segura em <data>" ("declared themselves safe on <date>") and the family's contact); without it, match stored as `declined_in_advance`, with no notice.
8. Record without an event: deleted 30 days after the record.
9. **Q15 (postponed):** limits for public captures (per IP or per device, and total per day). The review recommends a per-device limit with captcha, not per IP.
10. Acceptance criteria: `/safe` shows `tel:193` and `tel:199` and there is no route or collection that stores a rescue request; without prior consent, match `declined_in_advance` and no notice.

### 16.2 Remote endorsement and confirmation chain [dec. k]
In the pilot: validator or in-person endorsement.
1. Admins of shelters with `can_endorse`, of the same event, see `GET /v1/events/{id}/shelters-to-endorse` (site photos, address, creator's name, distance).
2. `POST /v1/shelters/{id}/endorsements/remote {endorser_shelter_id}`.
3. With two endorsements from two different shelters, by two different volunteers, none linked to the new shelter: `confirmed`, `confirmation_type = remote`, `can_endorse = false` until it receives an in-person or validator endorsement.
4. **Q30 (postponed):** "linked to the new shelter" = having or having had a `membership` with it.
5. **Q20 (postponed):** revocation does not affect the shelters the revoked one endorsed; it only flags who endorsed it.

### 16.3 Shelter and event merge [dec. i, Q2; dec. k]
In the pilot: detection and warning only; Civil Defense resolves by hand (3.2.1, 4.3).
- **Shelter merge (request):** in the offline conflict, "Fundir meus cadastros no abrigo X" ("Merge my registrations into shelter X") creates the source as `unconfirmed` and a request in `merges/{merge_id}` (`source_shelter_id`, `target_shelter_id`, `requested_by`, `status: pending|accepted|declined`, `decided_by?`, `created_at`, `decided_at?`). Routes `POST /v1/shelters/{id}/merges {target_shelter_id}` and `POST /v1/merges/{id}/decision {decision: accept|decline}` (destination admin, `confirmed` shelter). On acceptance, people, households, cards, deliveries and rescues go to the destination (the token resolves duplicates); if there was already an open `stay` at the destination, it keeps the one with the oldest `entered_at`; the source becomes `status = merged`, `merged_into = destination`; queue items arriving later for the source are applied to the destination. Before accepting, the destination sees nothing of the source.
- **Q40 (postponed):** on an accepted merge, the source's admins become volunteers at the destination (`via = merge`).
- **Event merge:** when a shelter reaches two or more events, the oldest survives; the others become `status = merged`, `merged_into`; shelters, searches, rescues and `people.event_ids` are repointed in batches, under the same global lock as shelter creation.
- **Distant shelter:** on creation, if there are active events in the region or state and none within ≤ R km, the app asks "Este abrigo recebe pessoas de algum destes eventos?" ("Does this shelter receive people from any of these events?") (`GET /v1/events/nearby`, `declared_event_ids`, `event_link = declared`) [dec. c].
- **Q31 (postponed):** the declared shelter takes part in the chain rule like any other; "region or state" = same state (UF) or ≤ 300 km.

### 16.4 Site photos [dec. k]
- Up to 2 site photos (without people), reduced to ≤ 200 KB (longest side 1024 px), in the subcollection `shelters/{id}/photos/{n}` (`image: bytes`, `content_type`, `uploaded_by`, `uploaded_at`), used in remote endorsement.
- Routes `POST /v1/shelters/{id}/photos {image_base64}` and `GET /v1/shelters/{id}/photos/{n}` (shelter admin, eligible endorser, Val, DC).
- **Q29 (postponed):** quantity, size and storage in Firestore itself.
