# Fase 3 — Contrato entre os engenheiros

Fundação (estágio 0) pronta na branch `phase-3-check-in`. Ela só tem tipos, rotas registradas e telas vazias: **nenhuma regra de negócio**. Cada dono preenche os arquivos da sua área. Leia também o [doc da fase](fase-3-check-in.md).

## Donos

| Dono | Área |
|---|---|
| A | API: pessoas e miniatura |
| B | API: núcleos e cartões |
| C | API: criança, saídas e lotação |
| D | API: modo degradado, `413`/`429`, CLI |
| E | PWA: consentimento, check-in, câmera, imagem, pessoa, saída, lotação |
| F | PWA offline: cifra, fila, assumir pendências, conflitos 9.4, telas de núcleo e cartão, QR, faixa degradada |

## API: módulo → rotas → tipos

Todas as rotas abaixo já respondem `401` sem sessão e `501 not_implemented` com sessão. O dono troca o corpo do handler (e o `#![allow(dead_code)]` do topo do módulo some quando o módulo estiver todo ligado).

| Dono | Arquivo | Rotas | Tipos já definidos |
|---|---|---|---|
| A | `people.rs` | `POST /v1/shelters/{id}/checkins`, `GET /v1/shelters/{id}/people`, `GET /v1/shelters/{id}/people/needs-selfie`, `GET`/`PATCH /v1/people/{person_id}`, `PUT /v1/people/{person_id}/biometrics` | `PersonDoc`, `Level`, `ChildStatus`, `Guardian`, `GuardianLink`, `Needs` e `Vulnerabilities` (recusam campo desconhecido, critério 38), `Stay`, `ExitReason`, `ExitDestination`, `Origin` |
| A | `thumbnail.rs` | `GET /v1/people/{person_id}/thumbnail` | `AppState::thumbnail_key()` (chave AES-256-GCM, já validada na partida) |
| B | `households.rs` | `POST /v1/shelters/{id}/households`, `GET`/`PATCH /v1/households/{id}` | `HouseholdDoc`, `Pet` |
| B | `cards.rs` | `POST /v1/households/{id}/cards`, `GET /v1/cards/{code}`, `POST /v1/cards/{code}/cancel` | `CardDoc`, `CardStatus` |
| C | `children.rs` | `POST /v1/people/{person_id}/guardians` | usa `Guardian` de `people.rs` |
| C | `exits.rs` | `POST /v1/shelters/{id}/exits` | usa `Stay`, `ExitDestination` de `people.rs`; a lotação ("N de capacidade") sai das `stays` abertas e entra em `shelters::full_view` |
| D | `degraded.rs` | nenhuma rota própria | `StatusDoc` (`system/status`), `require_unico` (gate `503 unico_degraded`, hoje deixa passar), `resume` (CLI `taba unico resume`, o parse de `main.rs` já chama) |

Já existem para todos: `ApiError::not_implemented`, `unico_degraded`, `protection_role_required`, `revalidation_failed`, `guardian_not_verified`, `person_in_other_household`, `household_has_card` e `code_taken` (`error.rs`; `no_face`, `invalid_image` e `image_too_large` vêm de `UnicoError`); `AlertKind::{ChildSeparated, RevalidationFailed, ExceptionalExit, HouseholdConflict, UnicoLimit}`; `Purpose::{Checkin, Revalidation, Biometrics}` (`POST /v1/captures` ainda os recusa com `unsupported_purpose`: quem precisar do propósito libera o seu em `auth::post_captures`, **e é o único trecho de `auth.rs` que se mexe**); `THUMBNAIL_KEY` obrigatória na partida (32 bytes em base64) e `PILOT_RESPONSIBLE` lida do ambiente; índices `people (current_shelter_id, updated_at)` e `cards (household_id, status)`.

**Índice de `households.member_ids`:** é de campo único (array); o Firestore o cria sozinho. A API do Firestore recusa índice composto de um campo, então ele não está no `firestore.indexes.json`.

**Dúvidas estruturais para o orquestrador (nada foi decidido):**
1. Como a PWA recebe `PILOT_RESPONSIBLE` para a tela de consentimento? A spec 8.1 define `GET /v1/status` só com `{unico, reason?}`. Sugestão: acrescentar `pilot_responsible` ao `GET /v1/status` (público, sem dados pessoais). Hoje nenhuma rota o devolve.
2. `PersonDoc.thumbnail_encrypted` é `Vec<u8>`; o dono A confere como o Firestore o grava (precisa ser valor `bytes`).

## PWA: arquivo → dono

| Dono | Arquivos |
|---|---|
| E | `screens/Checkin.tsx`, `screens/People.tsx`, `screens/Person.tsx`, `screens/Exit.tsx`, `screens/Privacy.tsx`; novos: `camera.ts`, `image.ts`, componentes do consentimento e do check-in; chaves `consent.*`, `checkin.*`, `person.*`, `child.*`, `exit.*` (a interface de criança fica em `Checkin`/`Person`/`Exit`) |
| F | `screens/Household.tsx`; novos: `crypto.ts`, assumir pendências, conflitos 9.4, leitura de QR (`qr.ts` já existe: acrescentar a leitura), faixa degradada; `queue.ts` e `queue.test.ts`; chaves `household.*`, `card.*`, `degraded.*`, `queue.*` |

Já prontos para todos:
- `routes.ts`: `shelter_checkin`, `shelter_people`, `shelter_household` (com `household_id`), `shelter_exit`, `person`, `privacy`; `back.ts` já sabe o "Voltar" delas; `app.tsx` já tem os `case` das telas.
- `api-phase3.ts`: tipos de pedido e resposta de todas as rotas novas (as respostas que a spec não fixa são **provisórias**: o dono da rota ajusta junto com a API); `api.ts`: um método por rota, e `createCapture(purpose, {secret, shelter_id, expected_person_id})` para os propósitos novos.
- `db.ts` versão 2: `BlobRecord` (foto cifrada: `iv`, `data`), `PersonRecord`, `HouseholdRecord`, `CardRecord`, índices `by_shelter`/`by_household`; a `photo_key` é a chave `photo_key` do store `config`. A migração de 1 para 2 tem teste.
- i18n: `pt-BR.json` e `en.json` têm uma chave-marcador `__<área>__` por seção (`consent`, `checkin`, `person`, `household`, `card`, `child`, `exit`, `degraded`, `queue`) e `__end__`; `es.json` tem só `consent.*`, **com textos provisórios** (rascunho a revisar com quem fala espanhol). `keys.test.ts` confere a paridade e que cada chave fica na seção do seu prefixo. A chave `degraded.banner` antiga continua no topo do arquivo; as novas de `degraded.*` entram depois do marcador.

## Regras de convivência

1. Cada um só edita os arquivos da sua tabela. Helper novo vai para o arquivo do dono do assunto (ou para um arquivo novo seu).
2. Arquivos compartilhados mudam só no necessário: `main.rs` (cada rota já está registrada; só acrescente `.route(...)` se a sua rota for nova), `api.ts`/`api-phase3.ts` (só os métodos e tipos das suas rotas), `routes.ts`, `db.ts` (mudança de schema exige versão nova e teste de migração), `app.tsx`, `permissions_tests.rs` e os JSON de i18n (**só dentro da seção do seu prefixo**, logo depois do marcador).
3. Matriz de permissões: cada dono tira as suas rotas de `PHASE_3_ROUTES` (`permissions_tests.rs`) e cria o teste de matriz real delas num módulo próprio.
4. Testes: nenhum chama a Unico; use o servidor falso (`UNICO_URL`). Escrita que depende de unicidade usa transação com leitura dentro dela e teste de concorrência (N pedidos, exatamente um sucesso).
5. Commits pequenos, em inglês, com a atribuição de sempre. Antes de cada commit: `git status` e `git diff --stat`.
