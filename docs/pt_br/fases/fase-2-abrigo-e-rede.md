# Fase 2 — Abrigo e rede

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Voluntário cria um abrigo no local (GPS com precisão, endereço pelo Nominatim, regra dos 100 m), o abrigo entra num evento automático, o admin convida voluntários por QR ou link, e o abrigo é confirmado por validador ou por aval presencial.

**Entrega demonstrável em produção:** criar um abrigo em campo aberto, ver o endereço preenchido e o evento "Cidade/UF – mês/ano"; tentar criar outro a menos de 100 m e receber o aviso com o primeiro nome do criador; convidar um segundo voluntário por QR e outro por link; um validador (papel dado por CLI na fase 1) confirma o abrigo; um admin de abrigo confirmado dá aval presencial a outro abrigo.

## Escopo
**Abrigos (3.2, 3.2.1, 4.2, 4.5)**
- `POST /v1/shelters` com UUID do cliente, direto ou pela fila: valida `accuracy ≤ 100`; procura abrigos `open` a ≤ `SHELTER_RADIUS_M` (geohash com 9 células + haversine, 6.3.3) sob o lock global (6.2.4); `409 shelter_nearby` com `first_name` e `phone` só se `show_phone`; com `other_site_reason` cria `flagged` e alerta `shelter_flagged`.
- No piloto, a única saída do conflito é "É outro local? Explique."; a DC resolve à mão (3.2.1). Itens da fila do abrigo em conflito ficam `held` (9.3).
- `capacity` opcional na criação e em `PATCH` (4.2).
- `GET`, `PATCH`, `POST .../close` (8.2).
- PWA: geolocalização com `enableHighAccuracy` e aviso se `accuracy > 100 m` (6.1.5); Nominatim reverse direto do celular, 1 requisição/s, cache e pendência offline em `addresses` com `PATCH` pela fila (6.1.6).

**Eventos automáticos (4.3)**
- Ao criar ou sincronizar um abrigo: nenhum evento ativo a ≤ `EVENT_RADIUS_KM` → evento novo com nome automático (refeito quando o endereço chegar, se `auto_named`); um → entra; dois ou mais → entra no do abrigo mais próximo e gera `event_overlap` para a DC (sem fusão no piloto).
- `last_activity_at` atualizado no máximo uma vez por hora.
- `PATCH /v1/events/{id}`: `name` (admin de abrigo do evento ou DC) e `reference_contacts` (validador ou DC). Os contatos de referência são usados pelos alertas de criança da fase 3.
- O encerramento automático de evento é da fase 4.

**Convites, membros e papéis no abrigo (3.3, 4.4, 2.3)**
- Convite QR (30 min, admin ou voluntário) e link (24 h, só voluntário), uso único, criados no servidor; aceite sempre com captura com liveness (`purpose = invite`), que é a revalidação de quem já tem perfil.
- Membros: listar, promover a admin, remover voluntário, sair; admin não remove admin; último admin não sai; DC remove qualquer um e bane da rede; remoção gera `volunteer_removed` para admins dos outros abrigos da pessoa.
- Removido de um abrigo só volta por QR presencial.

**Confirmação (3.9, 2.2)**
- Validador ou DC confirmam e revogam; revogação sinaliza avalistas (`endorser_flagged`).
- Aval presencial: código de 10 min gerado pelo admin do abrigo novo; avalista de abrigo `confirmed` com `can_endorse`, sem ser membro ativo do abrigo novo; dois celulares a ≤ `SHELTER_RADIUS_M`, ambos com `accuracy ≤ 100`.
- Visibilidade do abrigo não confirmado (4.2): só o que ele cadastrou.

**Alertas e idempotência**
- Coleção `alerts` (7) com `GET /v1/alerts` e `POST /v1/alerts/{id}/resolve` (8.8), já com o campo `informational`.
- Idempotência no servidor por `Idempotency-Key` (8, `idempotency`), com TTL de 7 dias: a primeira escrita offline real (criação de abrigo) chega nesta fase.

**PWA**
- Telas `/shelters/new`, `/shelter/:id` (barra do abrigo do design), `/shelter/:id/members`, `/invite/:code`, `/endorse/:code`, `/inbox` (alertas).
- Geração de QR com `qrcode` (6.1).

## Fora de escopo
Pessoas e check-in (fase 3); vagas e lotação na tela (fase 3, que cria as `stays`); `GET /v1/shelters/nearby` (fase 4); encerramento automático de evento (fase 4); exportação (fase 6); aval remoto, fusão de abrigos e de eventos, fotos do local e pergunta do "abrigo distante" (seção 16, depois do piloto).

## Seções da spec
2.2, 2.3, 3.2, 3.2.1, 3.3, 3.9, 4.2, 4.3 (itens 1–3 e 6), 4.4, 4.5, 6.1.5, 6.1.6, 6.2.4, 6.3.3, 7 (`events`, `shelters`, `memberships`, `invites`, `endorsement_codes`, `alerts`, `idempotency`), 8.2 (menos `nearby`), 8.3, 8.4 (menos papéis globais), 8.8 (alertas), 9.2, 9.3, 9.4, 10.2.

## Critérios de aceite da seção 15
- **8** — `accuracy` 101 m → `422 insufficient_accuracy`.
- **9** — abrigo a 80 m → `409 shelter_nearby` (telefone só com `show_phone`); com `other_site_reason` → `flagged`, alerta e contagem (a contagem aparece no painel da fase 6; aqui basta o campo `flagged` consultável).
- **10** — 40 km entra no evento; 60 km cria outro; dois eventos alcançados → evento do abrigo mais próximo e `event_overlap`, sem fundir.
- **12** — duas criações simultâneas a 50 m: só uma sem conflito.
- **14** — aval com celulares a 150 m → `422 too_far`; avalista membro ativo do abrigo novo → `403 cannot_endorse`.
- **15** — revogação volta a `unconfirmed` e sinaliza avalistas.
- **17** — link com `role = admin` → `422 link_volunteer_only`.
- **18** — QR usado duas vezes → `410`; QR com 31 min → `410`.
- **19** — admin removendo admin → `403`; último admin saindo → `409 last_admin`.
- **20** — banido não cria sessão nem aceita convite.
- **21** — removido não volta por link, volta por QR.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Coleções | `events`, `shelters`, `memberships`, `invites`, `endorsement_codes`, `alerts`, `idempotency` |
| Rotas | `POST/GET/PATCH /v1/shelters[/{id}]`, `POST /v1/shelters/{id}/close`, `PATCH /v1/events/{id}`, `POST /v1/shelters/{id}/confirm`, `POST /v1/shelters/{id}/revoke`, `POST /v1/shelters/{id}/endorsements/in-person/code`, `POST /v1/endorsements/in-person`, `POST/GET /v1/shelters/{id}/invites`, `DELETE /v1/invites/{code}`, `GET /v1/invites/{code}`, `POST /v1/invites/{code}/accept`, `GET /v1/shelters/{id}/members`, `PATCH/DELETE /v1/shelters/{id}/members/{volunteer_id}`, `POST /v1/shelters/{id}/leave`, `POST /v1/volunteers/{volunteer_id}/ban`, `GET /v1/alerts`, `POST /v1/alerts/{id}/resolve`; `POST /v1/captures` com `purpose: invite` |
| Alertas | `shelter_flagged`, `event_overlap`, `volunteer_removed`, `endorser_flagged` |
| Erros | `insufficient_accuracy`, `shelter_nearby`, `too_far`, `cannot_endorse`, `link_volunteer_only`, `invite_invalid`, `already_member`, `removed_from_shelter`, `admin_cannot_remove_admin`, `last_admin` |
| Variáveis | `SHELTER_RADIUS_M`, `EVENT_RADIUS_KM` |
| PWA | `/shelters/new`, `/shelter/:id`, `/shelter/:id/members`, `/invite/:code`, `/endorse/:code`, `/inbox` |
| Índices | `shelters (status, geohash)`, `shelters (event_id, status)`, `memberships (volunteer_id, status)`, `memberships (shelter_id, status)`, `alerts (resolved, informational, created_at)`; TTL `idempotency.expires_at` |

## Dependências
Fase 1 (sessão, extrator `Volunteer`, captura com liveness, CLI de papéis para ter um validador e uma DC em produção, auditoria).

## Testes
**Unitários (obrigatórios):** haversine; escolha de precisão do geohash e as 9 células (incluindo bordas de célula e meridiano); regra dos 100 m; regra do evento (0, 1, 2+ eventos; nome automático; renomear quando o endereço chega); lock de criação concorrente; cada regra de convite (expiração, uso único, nível do link, removido, banido); cada regra de membro; matriz 2.3 para tudo que a fase expõe (abrigo `unconfirmed` x `confirmed`); aval presencial (distância, precisão, `can_endorse`, membro ativo); revogação; idempotência (mesma chave → mesma resposta, sem efeito duplo); fila com `409 shelter_nearby` retendo dependentes.

**Manuais com a Unico:** aceitar um convite QR e um link com a jornada (1 processo cada; esperado 2 a 4).

**Manuais sem a Unico:** criar abrigo em lugar aberto e dentro de prédio (ver o aviso de precisão); criar abrigo offline e sincronizar; aval presencial com dois celulares lado a lado e a mais de 100 m.

## Riscos e pontos de atenção
- GPS dentro de prédio costuma passar de 100 m de `accuracy`; a mensagem precisa ser clara.
- Política de uso do Nominatim: 1 requisição/s, sem uso em massa.
- O lock global só vale com uma instância (comentário `ponytail` da 6.2.4).
- Em produção, para testar confirmação, conceda `validator` ou `civil_defense` por CLI a um voluntário real.

## Checklist de pronto
- [ ] Criação de abrigo online e offline, com regra dos 100 m, Nominatim e evento automático.
- [ ] Convites QR e link, membros, banimento e remoção conforme a matriz.
- [ ] Confirmação por validador, aval presencial e revogação.
- [ ] Alertas listáveis e resolvíveis; idempotência no servidor.
- [ ] Índices e TTL aplicados pelo `provision.yml`.
- [ ] Critérios 8, 9, 10, 12, 14, 15, 17, 18, 19, 20 e 21 verificados; 61 e 62 conferidos de novo.
- [ ] Testes manuais com a Unico feitos e contados no PR.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
