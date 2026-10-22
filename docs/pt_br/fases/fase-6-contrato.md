# Fase 6 — Contrato do estágio 0

> Fundação para cinco engenheiros trabalharem em arquivos disjuntos. Cada módulo está declarado em `main.rs`, com as rotas registradas respondendo `501 not_implemented`. **Cada dono edita só os arquivos da sua linha**; o que é compartilhado já está pronto. Se precisar mudar o arquivo de outro dono, pare e peça ao orquestrador.

## API (Rust)

| Dono | Arquivo | Rotas | Structs / funções |
|---|---|---|---|
| A | `dashboard.rs` | `GET /v1/dashboard/events`, `GET /v1/dashboard/events/{id}`, `GET /v1/dashboard/events/{id}/daily.csv` | `EventSummary`, `ShelterToConfirm`, `EventDashboard`, `ShelterRow`, `Counts`, `ByLevel`, `Movement`, `OpenMatches`, `PilotIndicators` |
| B | `export.rs` | `GET /v1/shelters/{id}/export.csv` | `events.stats` (escrita no descarte, ver C) |
| C | `purge.rs` (e o passo em `jobs.rs`) | — | `PurgeReport`, `purge::run`; `delivery_purge_at` em `jobs.rs` ao encerrar o evento |
| D | `audit.rs` (e as revisões de `legal_basis`) | — | `Activity`, `audit::note_activity` (já chamada por `people::post_checkin` e `rescues::post_rescue`) |

Compartilhado e já pronto: `EventDoc.delivery_purge_at`, `purged_at`, `stats` e `EventStats` (`events.rs`); `DeliveryDoc.household_pseudonym` e `pseudonymized` (já existiam); `AlertKind::AnomalyVolume`, `AnomalyDistance` e `OutOfArea` (`alerts.rs`); `config.rs` (`DELIVERY_RETENTION_DAYS`, `ANOMALY_CHECKINS_PER_10MIN`, `ANOMALY_RESCUES_PER_10MIN` já existiam; `PURGE_ENABLED`, padrão `false`, é novo), todas agora em `env.j2`, `.env.example` e nas variáveis de exemplo; os índices `people (event_ids, level)`, `audit_log (shelter_id, at)`, `audit_log (volunteer_id, at)` e o TTL de `audit_log.expires_at` em `firestore.indexes.json`; `jobs::run_once` já chama `purge::run`.

### Regras que ligam os donos

- **A** responde com os structs de `dashboard.rs`; o tipo da PWA é `pwa/src/api-phase6.ts` (o contrato de formato). Validador (leitura) e DC; qualquer outro papel `403`. Contagens com `count()`; cache curto no servidor. `GET /v1/dashboard/events` traz, por evento, `unconfirmed_shelters` (o que a PWA usa para a lista de abrigos a confirmar). O CSV diário tem uma linha por abrigo, sem nome de pessoa nem `person_id`.
- **B** exige admin de abrigo `confirmed` (`403` para `unconfirmed` e para quem não é admin do abrigo); CSV sem miniatura nem `person_id`; auditado (`LegalBasis` da 10.1.1). A forma de `events.stats` é `EventStats`: C a grava no descarte; B só lê o que precisar.
- **C** só apaga com `config.purge_enabled`; desligado, conta e registra o que apagaria (log e auditoria), sem tocar em nada. Lotes de 400 escritas, teto de 15 000 exclusões por dia, retoma na hora seguinte. O descarte do evento grava `stats` antes de apagar, `status = purged` e `purged_at`. Pessoa com um evento vencido e outro ativo não é apagada. As entregas são pseudonimizadas (`person_id` e `card_code` apagados, `household_id` vira `household_pseudonym` aleatório por núcleo, `pseudonymized = true`) e apagadas no `delivery_purge_at`. Perfis de voluntário: 365 dias após `last_seen_at`, exceto banidos.
- **D** chama as anomalias por `audit::note_activity` (melhor esforço, depois da escrita, nunca falha o pedido): `anomaly_volume`, `anomaly_distance`, `out_of_area`, todas `informational = true`, para a DC e fora da caixa dos admins. A revisão de `legal_basis` cobre household, card, `alert.resolve`, `role.grant` e `role.revoke`, `event.close`, `thumbnail_purge` e o `taba unico resume`. Os testes de varredura provam que nenhuma rota da seção 16 existe e que nenhum documento contém token puro nem foto.
- Nenhuma escrita nova sem auditoria; nada que apague sem a chave `PURGE_ENABLED`.

### Testes

`permissions_tests.rs` tem uma linha provisória (`phase_6_routes_are_registered_and_ask_for_a_session`): 401 sem sessão e 501 com sessão. Cada dono troca as linhas das suas rotas pela matriz real no arquivo do próprio módulo e apaga a linha provisória. `tests/boot.rs` já pede as rotas novas ao binário real. Testes automáticos nunca chamam a Unico (FakeUnico); os do descarte rodam com a chave ligada.

## PWA

| Dono | Rota | Arquivo (stub) |
|---|---|---|
| F | `/dashboard` | `screens/Dashboard.tsx`; o atalho em `screens/Home.tsx` (validator e civil_defense); o botão "Exportar CSV" em `screens/Shelter.tsx` (admin de abrigo confirmado) |

- Tipos: `api-phase6.ts`. Cliente tipado em `api.ts`: `getDashboardEvents`, `getDashboardEvent` e, para os dois CSVs (arquivos, o chamador lê o blob), `getDailyCsv` e `getShelterExport`.
- A lista de abrigos a confirmar vem de `unconfirmed_shelters` e leva a `/shelter/:id`, onde já estão os botões de confirmar e revogar.
- i18n: marcadores `__dashboard__` e `__export__` em `pt-BR.json` e `en.json`, antes de `__end__`.
- IndexedDB: nada novo.

## Regras de convivência

As mesmas das fases 3 a 5: arquivos disjuntos, nada de refatoração fora da sua linha, commits pequenos em inglês, testes de concorrência onde a spec 6.2.4 pede, testes automáticos nunca chamam a Unico, nada da instalação no repo. Os itens "Rever ..." da fase (PIN, instalação opcional, regra da miniatura) são decisões do dono, não código.
