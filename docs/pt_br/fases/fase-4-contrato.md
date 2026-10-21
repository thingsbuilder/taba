# Fase 4 — Contrato do estágio 0

> Fundação para oito engenheiros trabalharem em arquivos disjuntos. Cada módulo já está declarado em `main.rs`, com as rotas registradas respondendo `501 not_implemented`. **Cada dono edita só os arquivos da sua linha**; tudo o que é compartilhado já está pronto. Se precisar mudar um arquivo de outro dono, pare e peça ao orquestrador.

## API (Rust)

| Dono | Arquivo | Rotas | Structs / funções |
|---|---|---|---|
| A | `rescues.rs` | `POST /v1/rescues`, `PATCH /v1/rescues/{id}`, `GET /v1/shelters/nearby` | `RescueDoc`; `repoint_person` (gancho, já chamado) |
| B | `searches.rs`, `matching.rs` | `POST /v1/searches` | `SearchDoc`, `SearchVia`, `SearchStatus`, `MissingPerson`, `Family`, `SearchOwner`, `SearchInput`, `RegisterOutcome`; `register_photos`; `on_person_registered` |
| C | `matches.rs` | `GET /v1/matches`, `GET /v1/matches/{id}`, `POST /v1/matches/{id}/decision` | `MatchDoc`, `MediatorKind`, `MatchStatus`, `DecidedBy` |
| D | `roles.rs`, `jobs.rs`, `events.rs`, `alerts.rs` | `POST /v1/volunteers/{id}/roles`, `DELETE /v1/volunteers/{id}/roles/{role}` | `ClosureReason`, `closed_at`/`closure_reason`/`purge_at` em `EventDoc`; `ensure_open`; `jobs::spawn`/`run_once`; regras de destinatário de `SearchExpiring` e `FamilySearchReceived` |
| E | `search_links.rs` | `POST /v1/search-links`, `DELETE /v1/search-links/{code}`, `GET /v1/search-links/{code}` (público), `POST /v1/search-links/{code}/submit` (público) | `SearchLinkDoc`, `LinkStatus` |

Compartilhado e já pronto (ninguém mexe): `error.rs` (`search_limit` 429, `match_already_decided` 409, `event_closed` 409, `search_link_invalid` 410, `not_implemented` 501), `AlertKind::SearchExpiring` e `AlertKind::FamilySearchReceived`, `config.rs` (`SEARCHES_PER_DAY` 10, `INACTIVITY_DAYS` 30, `PURGE_AFTER_DAYS` 30, já existiam; agora também em `env.j2` e nas variáveis de exemplo), índices e TTL em `infra/firestore/firestore.indexes.json`.

### Assinaturas compartilhadas

- `searches::register_photos(state, owner: SearchOwner, input: SearchInput) -> Result<RegisterOutcome, ApiError>`: um token por foto, guarda só os `person_id`, grava a busca, tenta o match na hora (`matching`), `503 unico_degraded` sem gravar nada. **B escreve; E chama** no submit do link (`owner.via = FamilyLink`, `owner.volunteer_id` = quem gerou o link, `owner.key` = o código do link, para não pagar duas vezes a mesma foto). A rota valida antes: 1 a 5 fotos, cada uma decodificada e até 800 KB.
- `matching::on_person_registered(db, tx, &person, person_id, shelter: Option<&ShelterDoc>)`: dentro da transação que registra a pessoa; procura buscas ativas (`person_ids` array-contains, `status = active`) e cria `matches/{sha256(search_id|person_id)}` de forma idempotente, com a fila da spec 4.8. **B escreve.** Já é chamada por `people::checkin_in` (todo check-in) e `people::migrate_in` (biometria tardia). **A chama** no `POST /v1/rescues`. Pessoa `no_biometrics` não casa com nada.
- `rescues::repoint_person(db, tx, old, new)`: chamada por `people::migrate_in`. `people::repoint_person_refs` já move `rescues.person_id`; A confere e, se quiser, leva a lógica para cá.
- `events::ensure_open(db, event_id) -> Result<(), ApiError>`: `409 event_closed` quando o evento está encerrado. **D escreve o corpo e liga nas rotas de escrita de abrigo** (check-in, núcleo, cartão, saída, biometria tardia, resgate, entrega); A chama no resgate quando houver `event_id`.

### Índices

`searches (person_ids ARRAY_CONTAINS, status)`, `searches (event_id, status)`, `matches (mediator_kind, mediator_shelter_id, status, created_at DESC)` e, a mais, `matches (mediator_kind, status, created_at DESC)`: o documento sem `mediator_shelter_id` (filas `central` e `protection`) não entra no índice de quatro campos. TTL: `search_links.expires_at`.

### Testes

`permissions_tests.rs` tem duas linhas provisórias (`phase_4_routes_are_registered_and_ask_for_a_session` e `the_two_public_search_link_routes_need_no_session`): 401 sem sessão e 501 com sessão; as públicas 501 sem sessão. Cada dono troca as linhas das suas rotas pela matriz real (quem pode, quem leva 403) no arquivo do próprio módulo e apaga a linha provisória. `tests/boot.rs` já pede as rotas novas ao binário real.

## PWA

| Dono | Rota | Arquivo (stub) |
|---|---|---|
| F | `/rescue`, `/search` | `screens/Rescue.tsx`, `screens/Search.tsx` (mais `queue.ts`, `crypto.ts` e `Home.tsx` para fotos múltiplas, `search_limit` e os botões) |
| G | `/matches/:id`, `/roles`, matches na `/inbox` | `screens/MatchCheck.tsx`, `screens/Roles.tsx`, `screens/Inbox.tsx` |
| H | `/f/:code` (pública) | `screens/Family.tsx` |

- Tipos de todas as rotas: `api-phase4.ts`. **É o contrato de formato entre a API e as telas**: quem muda uma resposta muda o tipo e avisa o orquestrador. Cliente tipado em `api.ts` para todas as rotas (as duas do link da família, `getSearchLink` e `submitSearchLink`, não levam sessão).
- `/f/:code` fica fora do shell em `app.tsx` (`App` decide, `Shell` é o resto): sem login, PIN, barra de topo, fila nem instalação.
- `/search` responde por `queue`: o `429` do `queue.ts` hoje repete com espera; `search_limit` não pode repetir (F decide: `needs_action`).
- IndexedDB: nada novo foi preciso, então não há versão nova. A fila e os blobs cifrados (`queue`, `blobs`) servem; várias fotos por item são com F (`PhotoFields`).
- i18n: um marcador por seção em `pt-BR.json` e `en.json`, antes de `__end__`: `__rescue__` (F), `__search__` (F), `__family__` (H), `__match__` (G), `__role__` (G), `__inbox__` (G, chaves `inbox.*` novas). `es.json` só tem `consent.*` e `family.*`; `family.*` tem de existir completo em pt-BR, en e es (teste). Códigos de erro novos já têm texto (`error.search_limit`, `error.match_already_decided`, `error.search_link_invalid`).
- Rotas novas em `routes.ts` com teste; `/matches/:id` volta para `/inbox`.

## Decisões fechadas

1. **Aviso "a família enviou":** `AlertKind::FamilySearchReceived` (`family_search_received`), informativo, `volunteer_ids = [quem gerou o link]`; mostra só "a família enviou" e `usable_photos`, nunca o conteúdo.
2. **Modelo de dados** (já na spec 7, pt_br e en): `searches.created_by` + `via: volunteer|family_link`; `search_links/{code}` com `created_by`, `shelter_id`, `event_id?`, `status`, `created_at`, `expires_at` (TTL), `used_at?`, `search_id?`, `attempts`.

## Para o dono D

Em `alerts.rs`, `GET /v1/alerts` hoje recusa (`403`) quem não é admin, `protection` ou DC; quem registra buscas e não é admin precisa receber `search_expiring` e `family_search_received`.

## Limites conhecidos da PWA de matches e papéis (G)

- `/roles` não tem busca de voluntário (não há rota para isso): quem concede cola o código do voluntário, que o próprio voluntário vê no Perfil ("Meu código de voluntário"). Uma busca por nome é trabalho futuro.
- A conferência `/matches/:id` não mostra a miniatura: `MatchDetail` só traz `has_thumbnail` e não há rota para a imagem de um match (`GET /v1/people/{id}/thumbnail` pede o `person_id`, que o match não expõe). Pendente de decisão: uma rota `GET /v1/matches/{id}/thumbnail` em `matches.rs`.
- Os textos dos alertas leem `detail.purge_at` (`search_expiring`) e `detail.usable_photos` (`family_search_received`); sem o campo, a data fica em branco e a contagem em 0. D confirma os nomes.

## Regras de convivência

As mesmas da fase 3: arquivos disjuntos, nada de refatoração fora da sua linha, commits pequenos em inglês, testes de concorrência onde a spec 6.2.4 pede, testes automáticos nunca chamam a Unico, nada da instalação no repo.
