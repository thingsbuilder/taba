# Fase 5 — Contrato do estágio 0

> Fundação para quatro engenheiros trabalharem em arquivos disjuntos. Cada módulo está declarado em `main.rs`, com as rotas registradas respondendo `501 not_implemented`. **Cada dono edita só os arquivos da sua linha**; o que é compartilhado já está pronto. Se precisar mudar o arquivo de outro dono, pare e peça ao orquestrador.

## API (Rust)

| Dono | Arquivo | Rotas | Structs / funções |
|---|---|---|---|
| A | `deliveries_admin.rs` | `PUT /v1/shelters/{id}/items`, `GET /v1/shelters/{id}/deliveries?status=`, `POST /v1/deliveries/{id}/review` | grava `shelters.items`; lista; revisão (`reviewed_ok` \| `irregular`, nota, `reviewed_by`) |
| B | `deliveries.rs` | `POST /v1/shelters/{id}/deliveries` | `DeliveryDoc`, `ItemSpec`, `ItemScope`, `Items`, `Verification`, `DeliveryStatus`, `Reason`, `ReasonKind` (já definidos); o sorteio, a decisão de pedir liveness, a duplicidade |

Compartilhado e já pronto: `error.rs` (`liveness_required` 428, `admin_approval_required` 403, `card_not_found` 404, `person_not_in_household` 404, `not_implemented` 501), `ShelterDoc.items` (em `shelters.rs`, vazio por padrão), `DELIVERY_SAMPLE_RATE` (já em `config.rs`; agora também em `env.j2` e nas variáveis de exemplo), os três índices de `deliveries` em `firestore.indexes.json`, a spec 7 e 8.6.

### Regras que ligam A e B

- `DeliveryDoc` (spec 7) é a verdade dos dois. A lê e atualiza só `status`, `reviewed_by`, `review_note`; B cria.
- A lista de A (`GET .../deliveries`) responde `{deliveries: [DeliveryView]}` (tipo em `pwa/src/api-phase5.ts`), com nomes e nunca o `person_id`; só admin do abrigo. `status` filtra pelo índice `(shelter_id, status, delivered_at)`.
- B lê `ShelterDoc.items` para saber `scope`, `period_days` (7 se ausente), `once_per_event`, `high_value`. Item que não está em `items` → B decide o erro (`422 invalid_request`, campo `item`).
- `people::repoint_person_refs` já aponta `deliveries.person_id` na biometria tardia (spec 4.12); B só confere com teste.
- `events::ensure_shelter_open` entra nas escritas de A e B (`409 event_closed`); `events::touch` depois da escrita (no máximo uma vez por hora).
- Quando B grava a entrega, a resposta é `201 {delivery_id, status, reasons}`; `reasons[].previous_shelter_name` só sai quando o abrigo da outra entrega é `confirmed`.
- Auditoria de B com `LegalBasis::FraudPrevention`; a aprovação do admin grava o `approved_by` e a auditoria com o nome do admin.

### Índices

`deliveries (person_id, item, delivered_at DESC)`, `(household_id, item, delivered_at DESC)`, `(shelter_id, status, delivered_at DESC)`. Campo único não vai no JSON.

### Testes

`permissions_tests.rs` tem uma linha provisória (`phase_5_routes_are_registered_and_ask_for_a_session`): 401 sem sessão e 501 com sessão. Cada dono troca as linhas das suas rotas pela matriz real no arquivo do próprio módulo e apaga a linha provisória. `tests/boot.rs` já pede as rotas novas ao binário real. Testes automáticos nunca chamam a Unico (FakeUnico).

## PWA

| Dono | Rota | Arquivo (stub) |
|---|---|---|
| C | `/shelter/:id/delivery` | `screens/Delivery.tsx` (e, se precisar, `queue.ts`, `Household.tsx` para o botão no cartão lido) |
| D | `/shelter/:id/items`, `/shelter/:id/review` | `screens/Items.tsx`, `screens/Review.tsx`, e o botão "Doações" em `screens/Shelter.tsx` |

- Tipos de todas as rotas: `api-phase5.ts` (o contrato de formato entre a API e as telas). Cliente tipado em `api.ts` (`putItems`, `postDelivery`, `listDeliveries`, `reviewDelivery`).
- Rotas em `routes.ts` (`shelter_delivery`, `shelter_items`, `shelter_review`); o Voltar vai para o abrigo.
- A entrega vai pela fila (`Idempotency-Key` = id do item da fila; `id` UUID do cliente). `428 liveness_required` e `403 admin_approval_required` não são repetidos: o item espera uma pessoa (C decide o fluxo no `queue.ts`).
- i18n: um marcador por seção em `pt-BR.json` e `en.json`, antes de `__end__`: `__delivery__` (C), `__items__` (D), `__review__` (D). Os textos de erro novos já existem (`error.liveness_required`, `error.admin_approval_required`, `error.person_not_in_household`).
- IndexedDB: nada novo foi preciso (a fila e o cache de núcleos e cartões servem), então não há versão nova.
- A barra inferior com "Doações" fica como follow-up; entra como botão na tela do abrigo.

## Regras de convivência

As mesmas das fases 3 e 4: arquivos disjuntos, nada de refatoração fora da sua linha, commits pequenos em inglês, testes de concorrência onde a spec 6.2.4 pede, testes automáticos nunca chamam a Unico, nada da instalação no repo.

## Decisões do engenheiro B (entrega)

- **`offline: true` no corpo.** O app marca a entrega feita sem sinal (ou com o Unico degradado no cliente). Servidor: sem sorteio; item `high_value` sem `admin_approval` vira `in_review` com `liveness_unavailable`; com `admin_approval` por admin, `ok`. O servidor também lê `system/status`: em modo degradado vale a mesma regra. O corpo aceita campos desconhecidos.
- **Idempotência pelo `id` do corpo.** O reenvio depois da selfie (`capture_id`) é chamada direta, sem `Idempotency-Key`: o mesmo `id` com o mesmo voluntário e abrigo devolve a mesma resposta `201`, sem consultar a Unico de novo; outro voluntário (ou outro abrigo) com o mesmo `id` recebe `409 delivery_id_taken`.
- **`428 liveness_required`** traz `detail: {reason: "high_value" | "sample", admin_approval_only: bool}`; `admin_approval_only` é `true` para pessoa `no_biometrics` (a selfie não resolve: só a aprovação do admin).
- **Revalidação que falha (`409 revalidation_failed`):** nenhuma entrega é gravada; a captura é consumida e a auditoria `delivery.revalidation_failed` é gravada.
- **Janela de duplicidade:** duas entregas do item a menos de `period_days` de distância (em qualquer ordem, por causa da fila offline); `once_per_event` não tem janela. Só contam entregas de eventos ativos (ou sem evento).
- **Auditoria:** `delivery.create` ou `delivery.admin_approval`, base `fraud_prevention`, com o `volunteer_id` de quem enviou (o admin, na aprovação): o nome sai do cadastro do voluntário, a auditoria não guarda nomes.
